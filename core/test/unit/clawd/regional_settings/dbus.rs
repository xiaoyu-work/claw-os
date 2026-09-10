use super::*;
use crate::clawd::authority::Uses;
use crate::clawd::protocol::BrokerErrorKind;
use crate::clawd::regional_settings::{
    complete_attempt, preflight,
    tests::{client, decision, request},
};
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};

struct Bus {
    child: Child,
    _directory: tempfile::TempDir,
    address: String,
}

impl Bus {
    fn new() -> Self {
        let directory = tempfile::Builder::new()
            .prefix("regional-bus-")
            .tempdir_in(std::env::current_dir().unwrap())
            .unwrap();
        let address = format!("unix:path={}/bus", directory.path().display());
        let mut child = Command::new("dbus-daemon")
            .args([
                "--session",
                "--nofork",
                "--nopidfile",
                "--nosyslog",
                "--print-address=1",
            ])
            .arg(format!("--address={address}"))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("private dbus-daemon");
        let mut line = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        assert!(line.starts_with(&address), "{line}");
        Self {
            child,
            _directory: directory,
            address,
        }
    }

    async fn connection(&self) -> Connection {
        zbus::connection::Builder::address(self.address.as_str())
            .unwrap()
            .build()
            .await
            .unwrap()
    }
}

impl Drop for Bus {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[derive(Default)]
struct State {
    locale: Vec<String>,
    language: String,
    hostname: String,
    resolved_uid: u32,
    looked_up: Vec<i64>,
    calls: Vec<&'static str>,
    interactive: Vec<bool>,
    refuse: bool,
    ambiguous: bool,
    simplify_locale: bool,
    delay_hostname: bool,
    fail_hostname_readback: bool,
    different_hostname_readback: bool,
}

impl State {
    fn before(&mut self, action: &'static str) -> zbus::fdo::Result<()> {
        self.calls.push(action);
        if self.refuse {
            return Err(zbus::fdo::Error::AccessDenied("fixture refusal".into()));
        }
        Ok(())
    }
    fn after(&self) -> zbus::fdo::Result<()> {
        if self.ambiguous {
            Err(zbus::fdo::Error::Failed(
                "fixture lost acknowledgement after write".into(),
            ))
        } else {
            Ok(())
        }
    }
}

struct Locale(Arc<Mutex<State>>);
#[zbus::interface(name = "org.freedesktop.locale1")]
impl Locale {
    fn set_locale(&self, values: Vec<String>, interactive: bool) -> zbus::fdo::Result<()> {
        let mut state = self.0.lock().unwrap();
        state.before("locale")?;
        state.interactive.push(interactive);
        state.locale = values;
        if state.simplify_locale {
            state.locale.retain(|value| value.starts_with("LANG="));
        }
        state.after()
    }
    #[zbus(property)]
    fn locale(&self) -> Vec<String> {
        self.0.lock().unwrap().locale.clone()
    }
}

struct Hostname(Arc<Mutex<State>>);
#[zbus::interface(name = "org.freedesktop.hostname1")]
impl Hostname {
    async fn set_static_hostname(&self, value: &str, interactive: bool) -> zbus::fdo::Result<()> {
        let (delay, result) = {
            let mut state = self.0.lock().unwrap();
            state.before("hostname")?;
            state.interactive.push(interactive);
            state.hostname = value.into();
            (state.delay_hostname, state.after())
        };
        if delay {
            tokio::time::sleep(CALL_TIMEOUT + Duration::from_secs(1)).await;
        }
        result
    }
    #[zbus(property)]
    fn static_hostname(&self) -> zbus::fdo::Result<String> {
        let state = self.0.lock().unwrap();
        if state.fail_hostname_readback {
            Err(zbus::fdo::Error::Failed("fixture readback refusal".into()))
        } else if state.different_hostname_readback {
            Ok("different-host".into())
        } else {
            Ok(state.hostname.clone())
        }
    }
}

struct Accounts(Arc<Mutex<State>>);
#[zbus::interface(name = "org.freedesktop.Accounts")]
impl Accounts {
    fn find_user_by_id(&self, uid: i64) -> zbus::zvariant::OwnedObjectPath {
        self.0.lock().unwrap().looked_up.push(uid);
        "/org/freedesktop/Accounts/UserFixture".try_into().unwrap()
    }
}

struct User(Arc<Mutex<State>>);
#[zbus::interface(name = "org.freedesktop.Accounts.User")]
impl User {
    fn set_language(&self, value: &str) -> zbus::fdo::Result<()> {
        let mut state = self.0.lock().unwrap();
        state.before("language")?;
        state.language = value.into();
        state.after()
    }
    #[zbus(property)]
    fn language(&self) -> String {
        self.0.lock().unwrap().language.clone()
    }
    #[zbus(property)]
    fn uid(&self) -> u64 {
        u64::from(self.0.lock().unwrap().resolved_uid)
    }
}

async fn services(bus: &Bus, state: Arc<Mutex<State>>) -> Connection {
    zbus::connection::Builder::address(bus.address.as_str())
        .unwrap()
        .name(LOCALE)
        .unwrap()
        .name(HOSTNAME)
        .unwrap()
        .name(ACCOUNTS)
        .unwrap()
        .serve_at("/org/freedesktop/locale1", Locale(state.clone()))
        .unwrap()
        .serve_at("/org/freedesktop/hostname1", Hostname(state.clone()))
        .unwrap()
        .serve_at("/org/freedesktop/Accounts", Accounts(state.clone()))
        .unwrap()
        .serve_at("/org/freedesktop/Accounts/UserFixture", User(state))
        .unwrap()
        .build()
        .await
        .unwrap()
}

#[test]
#[ignore = "requires isolated root invocation; uses only a private D-Bus fixture"]
fn root_private_bus_exact_effects_refusals_and_ambiguous_results() {
    assert_eq!(unsafe { libc::geteuid() }, 0);
    let _lock = crate::test_env::lock_env();
    let temp = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let _data = crate::test_env::TestEnvVarGuard::set("COS_DATA_DIR", temp.path().join("data"));
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let bus = Bus::new();
        let state = Arc::new(Mutex::new(State::default()));
        let services = services(&bus, state.clone()).await;
        let names = DBusProxy::new(&services).await.unwrap();
        for name in [LOCALE, HOSTNAME, ACCOUNTS] {
            assert_eq!(
                names
                    .get_name_owner(BusName::try_from(name).unwrap())
                    .await
                    .unwrap()
                    .as_str(),
                services.unique_name().unwrap().as_str(),
            );
        }
        let backend = Backend::from_connection(bus.connection().await)
            .await
            .unwrap();
        for body in [
            json!({"action":"system_locale","lang":"en_US.UTF-8","region":"de_DE.UTF-8"}),
            json!({"action":"owner_language","languages":"de_DE:de:en"}),
            json!({"action":"static_hostname","hostname":"fixture-host"}),
        ] {
            let request = request(body);
            let no_grant = decision(&request, vec![], "not-a-special-app", Uses::Budget(1));
            assert!(preflight(&request, &client(), &no_grant).is_err());
            let granted = decision(
                &request,
                vec![request.required_cap()],
                "not-a-special-app",
                Uses::Budget(1),
            );
            assert!(preflight(
                &request,
                &crate::clawd::client_identity::ClientIdentity::unknown(),
                &granted
            )
            .is_err());
            let mut wrong_owner = client();
            wrong_owner.uid = Some(1);
            assert!(preflight(&request, &wrong_owner, &granted).is_err());
            let mut forged = serde_json::to_value(&request).unwrap();
            forged["owner_uid"] = json!(0);
            assert!(Request::decode(forged).is_err());
            assert!(state.lock().unwrap().calls.is_empty());
            assert!(state.lock().unwrap().looked_up.is_empty());
        }
        {
            let connection = bus.connection().await;
            let unavailable = Backend::from_connection(connection.clone()).await.unwrap();
            connection.close().await.unwrap();
            let request = request(json!({"action":"static_hostname","hostname":"must-not-apply"}));
            let grant = decision(
                &request,
                vec![request.required_cap()],
                "not-a-special-app",
                Uses::Budget(1),
            );
            let owner = preflight(&request, &client(), &grant).unwrap();
            let error = complete_attempt(
                &request,
                &grant,
                unavailable.apply(&request, owner, &grant).await,
            )
            .unwrap_err();
            assert_eq!(error.kind, BrokerErrorKind::Unavailable);
            assert!(crate::clawd::authority::obligation_met(Some(&grant)));
            assert!(grant.require(request.required_cap()).is_err());
            assert!(state.lock().unwrap().calls.is_empty());
            assert!(state.lock().unwrap().looked_up.is_empty());
        }
        for (body, expected) in [
            (
                json!({"action":"system_locale","lang":"en_US.UTF-8","region":"de_DE.UTF-8"}),
                "locale",
            ),
            (
                json!({"action":"owner_language","languages":"de_DE:de:en"}),
                "language",
            ),
            (
                json!({"action":"static_hostname","hostname":"fixture-host"}),
                "hostname",
            ),
        ] {
            let request = request(body);
            let grant = decision(
                &request,
                vec![request.required_cap()],
                "not-a-special-app",
                Uses::Budget(1),
            );
            let owner = preflight(&request, &client(), &grant).unwrap();
            let before = state.lock().unwrap().calls.len();
            let result = backend.apply(&request, owner, &grant).await.unwrap();
            assert_eq!(result["action"], request.action());
            assert_eq!(result["status"], "applied");
            assert_eq!(state.lock().unwrap().calls[before..], [expected]);
        }
        {
            let state = state.lock().unwrap();
            assert_eq!(state.locale, locale_values("en_US.UTF-8", "de_DE.UTF-8"));
            assert_eq!(state.language, "de_DE:de:en");
            assert_eq!(state.hostname, "fixture-host");
            assert_eq!(state.looked_up, [0]);
            assert_eq!(state.interactive, [false, false]);
        }
        state.lock().unwrap().simplify_locale = true;
        let same_locale =
            request(json!({"action":"system_locale","lang":"en_US.UTF-8","region":"en_US.UTF-8"}));
        let grant = decision(
            &same_locale,
            vec![same_locale.required_cap()],
            "not-a-special-app",
            Uses::Budget(1),
        );
        let owner = preflight(&same_locale, &client(), &grant).unwrap();
        let result = backend.apply(&same_locale, owner, &grant).await.unwrap();
        assert_eq!(
            result["locale"],
            json!(locale_values("en_US.UTF-8", "en_US.UTF-8"))
        );
        assert_eq!(state.lock().unwrap().locale, ["LANG=en_US.UTF-8"]);

        let revoked = request(json!({"action":"static_hostname","hostname":"must-not-apply"}));
        let grant = decision(
            &revoked,
            vec![revoked.required_cap()],
            "not-a-special-app",
            Uses::Budget(1),
        );
        let owner = preflight(&revoked, &client(), &grant).unwrap();
        crate::clawd::authority::authority().revoke_session(revoked.session());
        let count = state.lock().unwrap().calls.len();
        assert_eq!(
            backend
                .apply(&revoked, owner, &grant)
                .await
                .unwrap_err()
                .kind,
            BrokerErrorKind::Unauthorized
        );
        assert_eq!(state.lock().unwrap().calls.len(), count);
        for ambiguous in [false, true] {
            state.lock().unwrap().refuse = !ambiguous;
            state.lock().unwrap().ambiguous = ambiguous;
            let request = request(json!({"action":"static_hostname","hostname":"second-host"}));
            let grant = decision(
                &request,
                vec![request.required_cap()],
                "not-a-special-app",
                Uses::Unbounded,
            );
            let owner = preflight(&request, &client(), &grant).unwrap();
            let error = backend.apply(&request, owner, &grant).await.unwrap_err();
            assert_eq!(
                error.kind,
                if ambiguous {
                    BrokerErrorKind::Indeterminate
                } else {
                    BrokerErrorKind::Unauthorized
                }
            );
            assert_eq!(
                state.lock().unwrap().hostname,
                if ambiguous {
                    "second-host"
                } else {
                    "fixture-host"
                }
            );
        }
        state.lock().unwrap().refuse = false;
        state.lock().unwrap().ambiguous = false;
        for readback_failure in [true, false] {
            state.lock().unwrap().fail_hostname_readback = readback_failure;
            state.lock().unwrap().different_hostname_readback = !readback_failure;
            let request = request(json!({"action":"static_hostname","hostname":"readback-host"}));
            let grant = decision(
                &request,
                vec![request.required_cap()],
                "not-a-special-app",
                Uses::Budget(1),
            );
            let owner = preflight(&request, &client(), &grant).unwrap();
            let count = state.lock().unwrap().calls.len();
            let error = backend.apply(&request, owner, &grant).await.unwrap_err();
            assert_eq!(error.kind, BrokerErrorKind::Indeterminate);
            assert_eq!(state.lock().unwrap().hostname, "readback-host");
            assert_eq!(state.lock().unwrap().calls.len(), count + 1);
        }
        state.lock().unwrap().fail_hostname_readback = false;
        state.lock().unwrap().different_hostname_readback = false;
        state.lock().unwrap().delay_hostname = true;
        let timed = request(json!({"action":"static_hostname","hostname":"timed-host"}));
        let grant = decision(
            &timed,
            vec![timed.required_cap()],
            "not-a-special-app",
            Uses::Budget(1),
        );
        let owner = preflight(&timed, &client(), &grant).unwrap();
        let count = state.lock().unwrap().calls.len();
        let started = tokio::time::Instant::now();
        let error = backend.apply(&timed, owner, &grant).await.unwrap_err();
        assert_eq!(error.kind, BrokerErrorKind::Indeterminate);
        assert!(started.elapsed() >= CALL_TIMEOUT);
        assert!(started.elapsed() < CALL_TIMEOUT + Duration::from_secs(2));
        assert_eq!(state.lock().unwrap().hostname, "timed-host");
        assert_eq!(state.lock().unwrap().calls.len(), count + 1);
        state.lock().unwrap().resolved_uid = 1000;
        let request = request(json!({"action":"owner_language","languages":"en"}));
        let grant = decision(
            &request,
            vec![request.required_cap()],
            "not-a-special-app",
            Uses::Unbounded,
        );
        let owner = preflight(&request, &client(), &grant).unwrap();
        let before = state.lock().unwrap().calls.len();
        assert_eq!(
            backend
                .apply(&request, owner, &grant)
                .await
                .unwrap_err()
                .kind,
            BrokerErrorKind::Unauthorized
        );
        assert_eq!(state.lock().unwrap().calls.len(), before);
    });
}

#[test]
fn non_root_private_bus_is_not_a_production_backend() {
    if unsafe { libc::geteuid() } == 0 {
        return;
    }
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let bus = Bus::new();
        assert!(Backend::from_connection(bus.connection().await)
            .await
            .is_err());
    });
}
