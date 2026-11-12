use std::cell::RefCell;
use std::fmt::Write as _;
use std::io::Write as _;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

#[derive(Clone, Copy)]
pub(crate) enum Step {
    Needs,
    RuntimeBefore,
    FreshTrust,
    RetainedBefore,
    VerifyPackage,
    PackageOpen,
    EnvelopeRead,
    EnvelopeParse,
    SignerContext,
    Signature,
    TreeScan,
    DirectoryInventory,
    FileOpen,
    FileRead,
    FileHash,
    TreeCompare,
    RetainedBinding,
    RetainedAfter,
    WithdrawalPackage,
    WithdrawalSession,
    RuntimeAfter,
    CeilingPolicy,
}

const STEPS: [&str; 22] = [
    "needs",
    "runtime_before",
    "fresh_trust",
    "retained_before",
    "verify_package",
    "package_open",
    "envelope_read",
    "envelope_parse_validate",
    "signer_context",
    "signature_canonical",
    "tree_scan",
    "directory_inventory",
    "file_open",
    "file_read",
    "file_hash",
    "tree_compare",
    "retained_binding",
    "retained_after",
    "withdrawal_package",
    "withdrawal_session",
    "runtime_after",
    "ceiling_policy",
];

#[derive(Clone, Copy, Default)]
struct Cost {
    elapsed_ns: u128,
    cpu_ns: Option<u128>,
    calls: u64,
}

struct Clock {
    start: Instant,
    cpu_start: Option<u128>,
}

impl Clock {
    fn new() -> Self {
        Self {
            start: Instant::now(),
            cpu_start: thread_cpu_ns(),
        }
    }

    fn elapsed(&self) -> Cost {
        Cost {
            elapsed_ns: self.start.elapsed().as_nanos(),
            cpu_ns: thread_cpu_ns()
                .zip(self.cpu_start)
                .and_then(|(end, start)| end.checked_sub(start)),
            calls: 1,
        }
    }
}

#[cfg(target_os = "linux")]
fn thread_cpu_ns() -> Option<u128> {
    let mut time = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    if unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut time) } != 0 {
        return None;
    }
    Some(u128::try_from(time.tv_sec).ok()? * 1_000_000_000 + u128::try_from(time.tv_nsec).ok()?)
}

#[cfg(not(target_os = "linux"))]
fn thread_cpu_ns() -> Option<u128> {
    None
}

struct Profile {
    id: u64,
    clock: Clock,
    costs: [Cost; STEPS.len()],
    directories: u64,
    files: u64,
    bytes: u64,
}

thread_local! {
    static ACTIVE: RefCell<Option<Profile>> = const { RefCell::new(None) };
}

pub(crate) struct Guard {
    previous: Option<Profile>,
}

impl Guard {
    pub(crate) fn enter() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let profile = Profile {
            id: NEXT.fetch_add(1, Ordering::Relaxed),
            clock: Clock::new(),
            costs: [Cost::default(); STEPS.len()],
            directories: 0,
            files: 0,
            bytes: 0,
        };
        Self {
            previous: ACTIVE.with(|active| active.replace(Some(profile))),
        }
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        let Some(profile) = ACTIVE.with(|active| active.replace(self.previous.take())) else {
            return;
        };
        let total = profile.clock.elapsed();
        let thread = std::thread::current();
        let lane = match thread.name() {
            Some("gui-file-checks") => "checker",
            Some("claw-native-fs") => "native_io",
            _ => "controller",
        };
        let mut line = String::new();
        let _ = write!(
            line,
            "{{\"sourceStep\":\"selected_declaration\",\"declarationSeq\":{},\"sourceLane\":\"{lane}\",\"traceMilliseconds\":{},\"elapsedUs\":{},\"threadCpuUs\":{},\"directories\":{},\"packageFiles\":{},\"packageBytes\":{},\"steps\":{{",
            profile.id, total.elapsed_ns / 1_000_000, total.elapsed_ns / 1_000,
            cpu_json(total.cpu_ns), profile.directories, profile.files, profile.bytes,
        );
        let mut separator = "";
        for (index, cost) in profile.costs.iter().enumerate() {
            if cost.calls == 0 {
                continue;
            }
            let _ = write!(
                line,
                "{separator}\"{}\":{{\"calls\":{},\"elapsedUs\":{},\"threadCpuUs\":{}}}",
                STEPS[index],
                cost.calls,
                cost.elapsed_ns / 1_000,
                cpu_json(cost.cpu_ns),
            );
            separator = ",";
        }
        line.push_str("}}");
        let _ = writeln!(
            std::io::stderr().lock(),
            "private selected-declaration profile {line}",
        );
    }
}

fn cpu_json(cpu_ns: Option<u128>) -> String {
    cpu_ns
        .map(|ns| (ns / 1_000).to_string())
        .unwrap_or_else(|| "null".into())
}

pub(crate) struct Span {
    id: u64,
    step: Step,
    clock: Clock,
}

impl Span {
    pub(crate) fn new(step: Step) -> Option<Self> {
        let id = ACTIVE.with(|active| active.borrow().as_ref().map(|profile| profile.id))?;
        Some(Self {
            id,
            step,
            clock: Clock::new(),
        })
    }
}

impl Drop for Span {
    fn drop(&mut self) {
        let cost = self.clock.elapsed();
        ACTIVE.with(|active| {
            let mut active = active.borrow_mut();
            let Some(profile) = active.as_mut().filter(|profile| profile.id == self.id) else {
                return;
            };
            let prior = &mut profile.costs[self.step as usize];
            prior.cpu_ns = if prior.calls == 0 {
                cost.cpu_ns
            } else {
                prior.cpu_ns.zip(cost.cpu_ns).map(|(a, b)| a + b)
            };
            prior.elapsed_ns += cost.elapsed_ns;
            prior.calls += 1;
        });
    }
}

pub(crate) fn directory() {
    ACTIVE.with(|active| {
        if let Some(profile) = active.borrow_mut().as_mut() {
            profile.directories += 1;
        }
    });
}

pub(crate) fn file(bytes: u64) {
    ACTIVE.with(|active| {
        if let Some(profile) = active.borrow_mut().as_mut() {
            profile.files += 1;
            profile.bytes += bytes;
        }
    });
}
