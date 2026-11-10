use super::*;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use zbus::{Guid, object_server::SignalEmitter};

struct Presenter {
    version: u32,
    muted: Arc<AtomicBool>,
    fail_write: Arc<AtomicBool>,
    calls: Arc<Mutex<Vec<String>>>,
}

#[zbus::interface(name = "com.clawos.NotificationPresentation1")]
impl Presenter {
    fn version(&self) -> u32 {
        self.version
    }

    fn preferences(&self) -> String {
        Preferences::new(self.muted.load(Ordering::SeqCst))
            .to_json()
            .unwrap()
    }

    fn set_preferences(&self, payload: &str) -> zbus::fdo::Result<String> {
        let value = Preferences::from_json(payload)
            .map_err(|error| zbus::fdo::Error::InvalidArgs(error.to_string()))?;
        if self.fail_write.load(Ordering::SeqCst) {
            return Err(zbus::fdo::Error::Failed("write denied".into()));
        }
        self.muted.store(value.muted, Ordering::SeqCst);
        Ok(self.preferences())
    }

    fn dismiss(&self, id: u32) {
        self.calls.lock().unwrap().push(format!("dismiss:{id}"));
    }

    fn invoke_action(&self, id: u32, action: &str) {
        self.calls
            .lock()
            .unwrap()
            .push(format!("action:{id}:{action}"));
    }

    #[zbus(signal)]
    async fn card(emitter: &SignalEmitter<'_>, payload: &str) -> zbus::Result<()>;
    #[zbus(signal)]
    async fn preferences_changed(emitter: &SignalEmitter<'_>, payload: &str) -> zbus::Result<()>;
    #[zbus(signal)]
    async fn failure(emitter: &SignalEmitter<'_>, message: &str) -> zbus::Result<()>;
}

async fn proxy(presenter: Presenter) -> (zbus::Connection, NotificationPresentationProxy<'static>) {
    let (server, client) = tokio::net::UnixStream::pair().unwrap();
    let server = Builder::socket(server)
        .p2p()
        .server(Guid::generate())
        .unwrap()
        .serve_at(claw_notification_presentation::OBJECT_PATH, presenter)
        .unwrap()
        .build();
    let client = Builder::socket(client).p2p().build();
    let (server, client) = tokio::try_join!(server, client).unwrap();
    let proxy = NotificationPresentationProxy::new(&client).await.unwrap();
    (server, proxy)
}

#[tokio::test]
async fn actual_public_client_requires_negotiation_and_confirmed_preferences_and_actions() {
    let muted = Arc::new(AtomicBool::new(false));
    let fail_write = Arc::new(AtomicBool::new(false));
    let calls = Arc::new(Mutex::new(Vec::new()));
    let (_server, proxy) = proxy(Presenter {
        version: VERSION,
        muted: Arc::clone(&muted),
        fail_write: Arc::clone(&fail_write),
        calls: Arc::clone(&calls),
    })
    .await;
    let (mut output, mut events) = futures::channel::mpsc::channel(10);
    let task = tokio::spawn(async move { client(&proxy, &mut output).await });
    assert!(matches!(
        events.next().await.unwrap(),
        Output::Preferences(Preferences { muted: false, .. })
    ));
    let Output::Ready(input) = events.next().await.unwrap() else {
        panic!("controls must be negotiated after preferences");
    };
    input.send(Input::SetMuted(true)).await.unwrap();
    assert!(matches!(
        events.next().await.unwrap(),
        Output::PreferenceResult(Ok(Preferences { muted: true, .. }))
    ));
    assert!(muted.load(Ordering::SeqCst));
    fail_write.store(true, Ordering::SeqCst);
    input.send(Input::SetMuted(false)).await.unwrap();
    assert!(matches!(
        events.next().await.unwrap(),
        Output::PreferenceResult(Err(_))
    ));
    assert!(muted.load(Ordering::SeqCst));
    input.send(Input::Dismiss(0)).await.unwrap();
    assert!(matches!(
        events.next().await.unwrap(),
        Output::RequestFailed(_)
    ));
    assert!(calls.lock().unwrap().is_empty());
    input.send(Input::Dismiss(7)).await.unwrap();
    assert!(matches!(events.next().await.unwrap(), Output::Dismissed(7)));
    assert_eq!(*calls.lock().unwrap(), ["dismiss:7"]);
    input
        .send(Input::Activated(7, "default".into()))
        .await
        .unwrap();
    tokio::time::timeout(CALL_TIMEOUT, async {
        while calls.lock().unwrap().len() != 2 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(*calls.lock().unwrap(), ["dismiss:7", "action:7:default"]);
    task.abort();
    let _ = task.await;
}

#[tokio::test]
async fn incompatible_presenter_never_enables_controls_or_falls_back_to_config() {
    let (_server, proxy) = proxy(Presenter {
        version: 2,
        muted: Arc::new(AtomicBool::new(true)),
        fail_write: Arc::new(AtomicBool::new(false)),
        calls: Arc::new(Mutex::new(Vec::new())),
    })
    .await;
    let (mut output, mut events) = futures::channel::mpsc::channel(10);
    assert!(
        client(&proxy, &mut output)
            .await
            .unwrap_err()
            .contains("does not support")
    );
    drop(output);
    assert!(events.next().await.is_none());
}
