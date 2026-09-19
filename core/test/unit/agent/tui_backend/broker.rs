use super::*;

#[tokio::test]
async fn broker_provider_honors_the_standard_socket_override() {
    struct Restore(Option<std::ffi::OsString>);
    impl Drop for Restore {
        fn drop(&mut self) {
            match self.0.take() {
                Some(value) => std::env::set_var(crate::clawd::config::SOCKET_ENV, value),
                None => std::env::remove_var(crate::clawd::config::SOCKET_ENV),
            }
        }
    }
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("isolated-clawd.sock");
    let _restore = Restore(std::env::var_os(crate::clawd::config::SOCKET_ENV));
    std::env::set_var(crate::clawd::config::SOCKET_ENV, &path);
    let mut config = CosConfig::default();
    config.agent.provider = "mock".into();
    config.agent.model = "mock".into();
    let backend = BrokerBackend::new(Arc::new(config), unsafe { libc::geteuid() })
        .await
        .unwrap();
    assert_eq!(backend.socket, path);
}
