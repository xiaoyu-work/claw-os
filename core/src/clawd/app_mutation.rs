//! Process-wide Root admission fencing, never a permission or an owner deny policy.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, Weak};

use tokio::sync::{OwnedRwLockReadGuard, OwnedRwLockWriteGuard, RwLock};

type Gates = Mutex<HashMap<String, Weak<RwLock<()>>>>;

fn gate(app: &str) -> Result<Arc<RwLock<()>>, String> {
    static GATES: OnceLock<Gates> = OnceLock::new();
    let mut gates = GATES
        .get_or_init(Default::default)
        .lock()
        .map_err(|_| "Root App mutation admission is unavailable")?;
    gates.retain(|_, gate| gate.strong_count() != 0);
    if let Some(gate) = gates.get(app).and_then(Weak::upgrade) {
        return Ok(gate);
    }
    let gate = Arc::new(RwLock::new(()));
    gates.insert(app.to_string(), Arc::downgrade(&gate));
    Ok(gate)
}

pub(crate) fn admit(app: &str) -> Result<OwnedRwLockReadGuard<()>, String> {
    gate(app)?
        .try_read_owned()
        .map_err(|_| format!("App `{app}` package mutation is pending; retry admission afterwards"))
}

pub(crate) async fn fence(app: &str) -> Result<OwnedRwLockWriteGuard<()>, String> {
    tokio::time::timeout(std::time::Duration::from_secs(10), gate(app)?.write_owned())
        .await
        .map_err(|_| format!("App `{app}` admission has not drained; package mutation remains pending"))
}
