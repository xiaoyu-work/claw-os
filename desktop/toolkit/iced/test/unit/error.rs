use super::*;

#[test]
fn assert_send_sync() {
    fn _assert<T: Send + Sync>() {}
    _assert::<Error>();
}
