use super::*;
use std::io::Write;

fn display_fixture(
    session: u32,
    epoch: Epoch,
    workload: Option<Arc<Workload>>,
) -> Arc<Mutex<Display>> {
    let (control, _requests) = mpsc::sync_channel(1);
    Arc::new(Mutex::new(Display {
        epoch,
        login: KernelLogin {
            owner_uid: 62050,
            session_id: session,
        },
        host: ProcessIdentity::current().unwrap(),
        wayland_display: Some("wayland-fixture".to_string()),
        workload,
        control,
        verified_retired: Arc::new(AtomicBool::new(false)),
    }))
}

#[test]
fn completed_retirement_releases_only_the_matching_registration() {
    let registry = Registry {
        displays: Mutex::new(HashMap::new()),
    };
    let display = display_fixture(1, Epoch([1; 16]), None);
    let unrelated = display_fixture(2, Epoch([2; 16]), None);
    registry.insert_display(1, display.clone()).unwrap();
    registry.insert_display(2, unrelated.clone()).unwrap();
    registry.finish_retirement(1, &display, || Ok(()));
    let entries = registry.displays.lock().unwrap();
    assert!(!entries.contains_key(&1));
    assert!(Arc::ptr_eq(&entries[&2], &unrelated));
}

#[test]
fn stale_retirement_cannot_release_a_replacement_registration() {
    let registry = Registry {
        displays: Mutex::new(HashMap::new()),
    };
    let stale = display_fixture(1, Epoch([1; 16]), None);
    let replacement = display_fixture(1, Epoch([2; 16]), None);
    registry.insert_display(1, replacement.clone()).unwrap();
    registry.finish_retirement(1, &stale, || Ok(()));
    assert!(Arc::ptr_eq(
        &registry.displays.lock().unwrap()[&1],
        &replacement,
    ));
}

struct PrivateRegistryCgroup {
    path: std::path::PathBuf,
    previous: OwnedFd,
    workloads: Vec<std::path::PathBuf>,
}

impl PrivateRegistryCgroup {
    fn new() -> Self {
        use claw_display_control::workload::{open_directory, write_file};

        assert_eq!(unsafe { libc::geteuid() }, 0);
        let membership = std::fs::read_to_string("/proc/self/cgroup").unwrap();
        let relative = membership
            .lines()
            .find_map(|line| line.strip_prefix("0::"))
            .unwrap();
        let previous =
            open_directory(&Path::new("/sys/fs/cgroup").join(relative.trim_start_matches('/')))
                .unwrap();
        let path = Path::new("/sys/fs/cgroup").join(format!(
            "claw-display-retirement-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple(),
        ));
        std::fs::create_dir(&path).unwrap();
        let descriptor = open_directory(&path).unwrap();
        write_file(
            descriptor.as_fd(),
            "cgroup.procs",
            std::process::id().to_string().as_bytes(),
        )
        .unwrap();
        println!("private retirement cgroup: {}", path.display());
        Self {
            path,
            previous,
            workloads: Vec::new(),
        }
    }

    fn workload(&mut self, epoch: Epoch) -> Arc<Workload> {
        let anchor = SessionGroup::of(&ProcessIdentity::current().unwrap()).unwrap();
        assert_eq!(anchor.path(), self.path);
        let path = self.path.join(format!("claw-display-{}", epoch.label()));
        std::fs::create_dir(&path).unwrap();
        self.workloads.push(path.clone());
        Arc::new(
            Workload::receive(
                claw_display_control::workload::open_directory(&path).unwrap(),
                &anchor,
                epoch,
            )
            .unwrap(),
        )
    }
}

impl Drop for PrivateRegistryCgroup {
    fn drop(&mut self) {
        if let Err(error) = claw_display_control::workload::write_file(
            self.previous.as_fd(),
            "cgroup.procs",
            std::process::id().to_string().as_bytes(),
        ) {
            eprintln!("private retirement test could not restore its own membership: {error}");
            return;
        }
        for path in self
            .workloads
            .iter()
            .rev()
            .chain(std::iter::once(&self.path))
        {
            if let Err(error) = std::fs::remove_dir(path) {
                if error.kind() != std::io::ErrorKind::NotFound {
                    eprintln!(
                        "private retirement cgroup cleanup failed at {}: {error}",
                        path.display()
                    );
                }
            }
        }
    }
}

struct RegistrySleeper(std::process::Child);

impl RegistrySleeper {
    fn in_workload(workload: &Workload) -> Self {
        let child = Self(
            std::process::Command::new("/usr/bin/sleep")
                .arg("30")
                .spawn()
                .unwrap(),
        );
        claw_display_control::workload::write_file(
            workload.as_fd(),
            "cgroup.procs",
            child.0.id().to_string().as_bytes(),
        )
        .unwrap();
        child
    }
}

impl Drop for RegistrySleeper {
    fn drop(&mut self) {
        if self.0.try_wait().is_ok_and(|status| status.is_none()) {
            if let Err(error) = self.0.kill() {
                eprintln!("private retirement sleeper kill failed: {error}");
            }
        }
        if let Err(error) = self.0.wait() {
            eprintln!("private retirement sleeper reap failed: {error}");
        }
    }
}

#[test]
#[ignore = "process fixture: requires Root and private cgroup v2 workload custody"]
fn private_deferred_retirement_reclaims_the_exact_slot_and_workload() {
    let mut cgroups = PrivateRegistryCgroup::new();
    let registry = Arc::new(Registry {
        displays: Mutex::new(HashMap::new()),
    });
    for session in 1..=MAX_DISPLAYS as u32 {
        let epoch = Epoch([session as u8; 16]);
        let workload = cgroups.workload(epoch);
        registry
            .insert_display(session, display_fixture(session, epoch, Some(workload)))
            .unwrap();
    }
    let display = registry.displays.lock().unwrap()[&1].clone();
    let weak_display = Arc::downgrade(&display);
    let weak_workload = Arc::downgrade(display.lock().unwrap().workload.as_ref().unwrap());
    let mut target = RegistrySleeper::in_workload(&weak_workload.upgrade().unwrap());
    let unrelated = registry.displays.lock().unwrap()[&2].clone();
    let mut other =
        RegistrySleeper::in_workload(unrelated.lock().unwrap().workload.as_ref().unwrap());
    let newcomer = display_fixture(99, Epoch([99; 16]), None);
    assert!(registry.insert_display(99, newcomer.clone()).is_err());
    let target_path = cgroups.workloads[0].clone();
    let (channel, _host) = Connection::pair().unwrap();
    let release = Arc::new(AtomicBool::new(false));
    let (attempts, observed) = mpsc::channel();
    let (done, finished) = mpsc::sync_channel(1);
    let service = registry.clone();
    let allowed = release.clone();
    let cleanup_display = display.clone();
    let owner = std::thread::spawn(move || {
        service.finish_retirement(1, &display, || {
            let result = retire_display(&cleanup_display, &channel);
            attempts
                .send((
                    Instant::now(),
                    result.as_ref().err().map(ToString::to_string),
                ))
                .unwrap();
            result?;
            if allowed.load(Ordering::Acquire) {
                Ok(())
            } else {
                Err(Error::Timeout)
            }
        });
        done.send(()).unwrap();
    });
    let (first, first_error) = observed.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(first_error.is_none(), "{first_error:?}");
    assert!(weak_display.upgrade().is_some());
    assert!(weak_workload.upgrade().is_some());
    assert!(GuiControl::of(&weak_display.upgrade().unwrap()).is_err());
    assert_eq!(registry.displays.lock().unwrap().len(), MAX_DISPLAYS);
    assert!(registry.insert_display(99, newcomer.clone()).is_err());
    assert!(target.0.try_wait().unwrap().is_some());
    assert!(other.0.try_wait().unwrap().is_none());
    std::fs::remove_dir(&target_path).unwrap();

    let (second, second_error) = observed
        .recv_timeout(Duration::from_secs(5))
        .expect("retirement owner disappeared after the first incomplete cleanup");
    assert!(
        second_error.is_none(),
        "a checked retired workload must not be reopened: {second_error:?}"
    );
    let (third, third_error) = observed
        .recv_timeout(Duration::from_secs(5))
        .expect("retirement must remain owned beyond the previous attempts");
    assert!(third_error.is_none(), "{third_error:?}");
    assert!(
        second.duration_since(first) >= Duration::from_millis(200),
        "busy retry loop"
    );
    assert!(
        third.duration_since(second) >= Duration::from_millis(200),
        "busy retry loop"
    );
    assert!(
        finished.try_recv().is_err(),
        "incomplete retirement reported completion"
    );
    assert!(weak_workload.upgrade().is_some());
    assert_eq!(registry.displays.lock().unwrap().len(), MAX_DISPLAYS);

    release.store(true, Ordering::Release);
    finished
        .recv_timeout(Duration::from_secs(5))
        .expect("late cleanup was not reclaimed");
    owner.join().unwrap();
    assert!(
        weak_display.upgrade().is_none(),
        "retired display reference leaked"
    );
    assert!(
        weak_workload.upgrade().is_none(),
        "retired workload descriptor leaked"
    );
    assert_eq!(registry.displays.lock().unwrap().len(), MAX_DISPLAYS - 1);
    assert!(Arc::ptr_eq(
        &registry.displays.lock().unwrap()[&2],
        &unrelated
    ));
    assert!(unrelated.lock().unwrap().wayland_display.is_some());
    assert!(
        other.0.try_wait().unwrap().is_none(),
        "unrelated workload was killed"
    );
    registry
        .insert_display(99, newcomer)
        .expect("completed retirement did not restore capacity");
    assert_eq!(registry.displays.lock().unwrap().len(), MAX_DISPLAYS);
}

#[test]
fn spontaneous_instance_end_cannot_consume_a_retirement_or_refresh_reply() {
    let epoch = Epoch([0x21; 16]);
    let instance = InstanceId([0x43; 16]);
    assert_eq!(
        reply_target(&CompositorReply::InstanceEnded { epoch, instance }, epoch).unwrap(),
        (instance, 5),
    );
    assert_eq!(
        reply_target(&CompositorReply::Retired { epoch, instance }, epoch).unwrap(),
        (instance, 3),
    );
    assert_eq!(
        reply_target(&CompositorReply::Refreshed { epoch, instance }, epoch).unwrap(),
        (instance, 2),
    );
    assert!(reply_target(
        &CompositorReply::InstanceEnded { epoch, instance },
        Epoch([0x22; 16]),
    )
    .is_err());
}

#[test]
#[ignore = "process fixture: requires its private Root mount namespace"]
fn private_registry_process() {
    assert_eq!(ProcessIdentity::current().unwrap().uid(), 0);
    assert_ne!(
        std::fs::read_link("/proc/self/ns/mnt").unwrap(),
        std::fs::read_link("/proc/1/ns/mnt").unwrap(),
    );
    let _registry = Registry::start().unwrap();
    println!("private display registry ready");
    std::io::stdout().flush().unwrap();
    let mut command = String::new();
    std::io::stdin().read_line(&mut command).unwrap();
    assert_eq!(command, "stop\n");
}
