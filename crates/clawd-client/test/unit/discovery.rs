use super::*;
use std::ffi::OsString;
use std::sync::Mutex;

static ENV_LOCK: Mutex<()> = Mutex::new(());

struct RestoreEnv(Vec<(&'static str, Option<OsString>)>);

impl RestoreEnv {
    fn clear() -> Self {
        let variables = [SOCKET_ENV, COMPAT_SOCKET_ENV, RUNTIME_ENV];
        let saved = variables
            .into_iter()
            .map(|variable| (variable, std::env::var_os(variable)))
            .collect();
        for variable in variables {
            std::env::remove_var(variable);
        }
        Self(saved)
    }
}

impl Drop for RestoreEnv {
    fn drop(&mut self) {
        for (variable, value) in self.0.drain(..) {
            if let Some(value) = value {
                std::env::set_var(variable, value);
            } else {
                std::env::remove_var(variable);
            }
        }
    }
}

#[test]
fn discovery_has_explicit_precedence_and_no_present_but_invalid_fallback() {
    let _lock = ENV_LOCK.lock().unwrap();
    let _restore = RestoreEnv::clear();

    assert_eq!(
        discover_socket().unwrap(),
        PathBuf::from(DEFAULT_SOCKET_PATH)
    );

    std::env::set_var(RUNTIME_ENV, "/run/test-cos");
    assert_eq!(
        discover_socket().unwrap(),
        PathBuf::from("/run/test-cos/clawd.sock")
    );

    std::env::set_var(COMPAT_SOCKET_ENV, "/run/compat.sock");
    assert_eq!(
        discover_socket().unwrap(),
        PathBuf::from("/run/compat.sock")
    );

    std::env::set_var(SOCKET_ENV, "/run/canonical.sock");
    assert_eq!(
        discover_socket().unwrap(),
        PathBuf::from("/run/canonical.sock")
    );

    std::env::set_var(SOCKET_ENV, "");
    assert!(matches!(
        discover_socket(),
        Err(ClientError::EmptySocketConfiguration {
            variable: SOCKET_ENV
        })
    ));
}
