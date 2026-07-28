use std::env;
use std::ffi::OsString;
use std::sync::{Mutex, MutexGuard, OnceLock};

use crate::caps::{CapSet, Role};
use crate::proc::{deregister_session, register_session, SessionInfo};
use crate::session::{self, SessionId, Status as SessionStatus};

pub struct ProcSessionGuard {
    session_id: String,
    previous_session: Option<OsString>,
    registered: bool,
    _env_lock: MutexGuard<'static, ()>,
}

impl ProcSessionGuard {
    pub fn enter(session_id: &SessionId, runtime: &str) -> Result<Self, String> {
        let env_lock = session_env_lock()
            .lock()
            .map_err(|_| "clawd session env lock poisoned".to_string())?;
        let previous_session = env::var_os("COS_SESSION");
        let session_id_string = session_id.as_str().to_string();
        let caps = session_caps(session_id)?;
        let role = session::get_meta(session_id)
            .ok()
            .and_then(|meta| meta.role)
            .unwrap_or(Role::Admin);

        session::update_meta(session_id, |meta| {
            if meta.status.is_active() {
                meta.status = SessionStatus::Running;
            }
            if meta.creator_runtime.is_none() {
                meta.creator_runtime = Some(runtime.to_string());
            }
        })
        .map_err(|err| err.to_string())?;

        let info = SessionInfo {
            session_id: session_id_string.clone(),
            pid: std::process::id(),
            command: vec![runtime.to_string()],
            started_at: chrono::Utc::now().to_rfc3339(),
            stdout_path: String::new(),
            stderr_path: String::new(),
            group: Some("clawd".to_string()),
            parent: None,
            workdir: env::current_dir()
                .ok()
                .map(|path| path.to_string_lossy().to_string()),
            exit_code: None,
            ended_at: None,
            tier: None,
            scope: Some("clawd-task".to_string()),
            priority: None,
            caps: Some(caps),
            role: Some(role.name().to_string()),
            app_id: None,
            pending_bind: false,
            start_time_ticks: None,
        };
        register_session(info)?;
        env::set_var("COS_SESSION", &session_id_string);

        Ok(Self {
            session_id: session_id_string,
            previous_session,
            registered: true,
            _env_lock: env_lock,
        })
    }
}

impl Drop for ProcSessionGuard {
    fn drop(&mut self) {
        if self.registered {
            deregister_session(&self.session_id);
        }
        match self.previous_session.take() {
            Some(value) => env::set_var("COS_SESSION", value),
            None => env::remove_var("COS_SESSION"),
        }
    }
}

fn session_caps(session_id: &SessionId) -> Result<CapSet, String> {
    let caps = session::get_caps(session_id).map_err(|err| err.to_string())?;
    if caps.is_empty() {
        return Err(format!(
            "session {} has no capabilities; refusing to enter clawd scope",
            session_id.as_str()
        ));
    }
    Ok(caps)
}

fn session_env_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}
