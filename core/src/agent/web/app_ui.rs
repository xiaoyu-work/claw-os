//! View-bound static resources. App frames never receive the Web credential.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;

use super::auth::AuthenticatedToken;
use crate::provenance::runtime::PackageRef;
use crate::provenance::VerifiedPackage;

#[derive(Clone)]
pub(super) struct View {
    owner_uid: u32,
    token_id: String,
    expires: Instant,
    pub package: Arc<VerifiedPackage>,
    pub expected: PackageRef,
    pub app: Value,
}

#[derive(Default)]
pub(super) struct Views {
    current: Mutex<HashMap<String, View>>,
}

impl Views {
    pub fn create(
        &self,
        caller: &AuthenticatedToken,
        package: Arc<VerifiedPackage>,
        expected: PackageRef,
        app: Value,
    ) -> Result<String, String> {
        let mut views = self
            .current
            .lock()
            .map_err(|_| "Web App views are unavailable")?;
        views.retain(|_, view| view.expires > Instant::now());
        if views.len() >= 32 {
            return Err(
                "Too many Web App views are open; close an App before opening another".into(),
            );
        }
        let mut bytes = [0_u8; 32];
        crate::credential::os_random_bytes(&mut bytes).map_err(|error| error.to_string())?;
        let id = hex::encode(bytes);
        views.insert(
            id.clone(),
            View {
                owner_uid: caller.uid,
                token_id: caller.token_id.clone(),
                expires: Instant::now() + Duration::from_secs(30 * 60),
                package,
                expected,
                app,
            },
        );
        Ok(id)
    }

    pub fn resource(&self, id: &str) -> Result<View, String> {
        if id.len() != 64 || !id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err("Unknown Web App view".into());
        }
        let view = self
            .current
            .lock()
            .map_err(|_| "Web App views are unavailable")?
            .get(id)
            .filter(|view| view.expires > Instant::now())
            .cloned()
            .ok_or("This Web App view has expired; reopen the App")?;
        view.package
            .assert_current(&crate::provenance::trust_store())
            .map_err(|_| {
                "The App package or trust changed; reopen its Web interface".to_string()
            })?;
        Ok(view)
    }

    pub fn owned(&self, id: &str, caller: &AuthenticatedToken) -> Result<View, String> {
        let view = self.resource(id)?;
        if view.owner_uid != caller.uid || view.token_id != caller.token_id {
            return Err(
                "This Web App view belongs to another authenticated browser session".into(),
            );
        }
        Ok(view)
    }

    pub fn close(&self, id: &str, caller: &AuthenticatedToken) -> Result<(), String> {
        self.owned(id, caller)?;
        self.current
            .lock()
            .map_err(|_| "Web App views are unavailable")?
            .remove(id);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/web/app_ui.rs"
    ));
}
