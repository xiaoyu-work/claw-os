use super::PasswdUser;

#[test]
fn passwd() {
    const EXAMPLE: &str =
        "speech-dispatcher:x:109:29:Speech Dispatcher,,,:/run/speech-dispatcher:/bin/false";

    assert_eq!(
        EXAMPLE.parse::<PasswdUser>(),
        Ok(PasswdUser {
            username: Box::from("speech-dispatcher"),
            uid: 109,
            full_name: Box::from("Speech Dispatcher")
        })
    );
}
