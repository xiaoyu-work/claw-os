//! Deterministic system-diagnosis orchestration.
//!
//! `cos agent diagnose` and the `cos_diagnose` agent tool use the same
//! read-only pipeline:
//!
//! 1. Route the symptom to a diagnostic domain.
//! 2. Select a bounded set of `cos_sysinfo` probes.
//! 3. Collect structured evidence.
//! 4. Apply conservative, explicit thresholds.
//! 5. Return findings with evidence references and confidence.
//!
//! The module does not ask an LLM to invent a diagnosis. The normal agent
//! loop can consume this report and explain it conversationally, while the
//! direct CLI remains useful on machines without a configured provider.

use serde::Serialize;
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::time::Instant;

const DOMAINS: &[&str] = &[
    "general",
    "performance",
    "network",
    "storage",
    "service",
    "crash",
    "thermal",
    "security",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum DiagnosticDomain {
    General,
    Performance,
    Network,
    Storage,
    Service,
    Crash,
    Thermal,
    Security,
}

impl DiagnosticDomain {
    fn as_str(self) -> &'static str {
        match self {
            Self::General => "general",
            Self::Performance => "performance",
            Self::Network => "network",
            Self::Storage => "storage",
            Self::Service => "service",
            Self::Crash => "crash",
            Self::Thermal => "thermal",
            Self::Security => "security",
        }
    }

    fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "general" | "all" => Some(Self::General),
            "performance" | "perf" | "cpu" | "memory" => Some(Self::Performance),
            "network" | "net" | "dns" => Some(Self::Network),
            "storage" | "disk" | "filesystem" | "fs" => Some(Self::Storage),
            "service" | "systemd" => Some(Self::Service),
            "crash" | "coredump" | "oom" => Some(Self::Crash),
            "thermal" | "temperature" | "battery" | "power" => Some(Self::Thermal),
            "security" | "auth" => Some(Self::Security),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
struct Options {
    symptom: String,
    domain: DiagnosticDomain,
    quick: bool,
    path: Option<String>,
}

#[derive(Debug, Clone)]
struct ProbeSpec {
    id: &'static str,
    command: &'static str,
    args: Vec<String>,
    description: &'static str,
}

#[derive(Debug, Clone, Serialize)]
struct Evidence {
    id: String,
    command: String,
    description: String,
    status: &'static str,
    latency_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
enum Severity {
    Info,
    Warning,
    Critical,
}

impl Severity {
    fn rank(self) -> u8 {
        match self {
            Self::Info => 0,
            Self::Warning => 1,
            Self::Critical => 2,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
struct Finding {
    code: &'static str,
    severity: Severity,
    title: String,
    detail: String,
    confidence: f64,
    evidence: Vec<String>,
    recommendation: String,
}

pub fn diagnose_cmd(args: &[String]) -> Result<Value, String> {
    if args
        .iter()
        .any(|arg| matches!(arg.as_str(), "--help" | "-h"))
    {
        return Ok(help());
    }
    diagnose_with(args, crate::sysinfo::run)
}

pub fn diagnose_primitive(_command: &str, args: &[String]) -> Result<Value, String> {
    diagnose_cmd(args)
}

fn diagnose_with<F>(args: &[String], runner: F) -> Result<Value, String>
where
    F: Fn(&str, &[String]) -> Result<Value, String>,
{
    let options = parse_options(args)?;
    let probes = plan_for(&options);
    let evidence = collect_evidence(&probes, runner);
    let mut findings = analyze(&evidence);
    let evidence_ids = evidence
        .iter()
        .map(|item| item.id.as_str())
        .collect::<BTreeSet<_>>();
    for finding in &mut findings {
        finding
            .evidence
            .retain(|evidence_id| evidence_ids.contains(evidence_id.as_str()));
    }
    findings.sort_by_key(|finding| std::cmp::Reverse(finding.severity.rank()));

    let successful = evidence.iter().filter(|item| item.status == "ok").count();
    let failed = evidence.len().saturating_sub(successful);
    let coverage = if evidence.is_empty() {
        0.0
    } else {
        successful as f64 / evidence.len() as f64
    };
    let confidence = round2(0.3 + 0.65 * coverage);
    let status = if successful == 0 {
        "fail"
    } else if findings
        .iter()
        .any(|finding| finding.severity == Severity::Critical)
    {
        "critical"
    } else if failed > 0
        || findings
            .iter()
            .any(|finding| finding.severity == Severity::Warning)
    {
        "warn"
    } else {
        "ok"
    };
    let recommendations = findings
        .iter()
        .map(|finding| finding.recommendation.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let summary = if findings.is_empty() {
        "No threshold-based issue was detected in the collected evidence.".to_string()
    } else {
        format!(
            "Detected {} finding(s): {} critical, {} warning, {} informational.",
            findings.len(),
            findings
                .iter()
                .filter(|finding| finding.severity == Severity::Critical)
                .count(),
            findings
                .iter()
                .filter(|finding| finding.severity == Severity::Warning)
                .count(),
            findings
                .iter()
                .filter(|finding| finding.severity == Severity::Info)
                .count(),
        )
    };

    serde_json::to_value(json!({
        "schema": 1,
        "status": status,
        "symptom": options.symptom,
        "domain": options.domain,
        "quick": options.quick,
        "generated_at": chrono::Utc::now().to_rfc3339(),
        "confidence": confidence,
        "coverage": {
            "planned": evidence.len(),
            "successful": successful,
            "failed": failed,
            "ratio": round2(coverage),
        },
        "summary": summary,
        "findings": findings,
        "recommendations": recommendations,
        "evidence": evidence,
        "interpretation": "Threshold findings are deterministic. Absence of a finding does not prove the system is healthy; inspect evidence and gather active probes when needed.",
    }))
    .map_err(|error| format!("serialize diagnosis report: {error}"))
}

fn help() -> Value {
    json!({
        "command": "cos agent diagnose",
        "summary": "Collect bounded system evidence and return confidence-linked findings.",
        "usage": "cos agent diagnose [--quick] [--domain <domain>] [--path <path>] <symptom>",
        "domains": DOMAINS,
        "examples": [
            "cos agent diagnose \"why is my computer slow?\"",
            "cos agent diagnose --domain network \"网络为什么很慢\"",
            "cos agent diagnose --domain storage --path /home/cos \"what is using disk space?\""
        ],
    })
}

fn parse_options(args: &[String]) -> Result<Options, String> {
    let mut quick = false;
    let mut explicit_domain = None;
    let mut path = None;
    let mut symptom_parts = Vec::new();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--quick" => {
                quick = true;
                index += 1;
            }
            "--domain" => {
                let raw = args
                    .get(index + 1)
                    .ok_or_else(|| "--domain requires a value".to_string())?;
                explicit_domain = Some(DiagnosticDomain::parse(raw).ok_or_else(|| {
                    format!(
                        "unknown diagnosis domain `{raw}`; expected {}",
                        DOMAINS.join(", ")
                    )
                })?);
                index += 2;
            }
            "--path" => {
                let raw = args
                    .get(index + 1)
                    .ok_or_else(|| "--path requires a value".to_string())?;
                path = Some(raw.clone());
                index += 2;
            }
            flag if flag.starts_with("--") => {
                return Err(format!(
                    "unknown diagnose flag: {flag}. supported: --quick | --domain <domain> | --path <path>"
                ));
            }
            value => {
                symptom_parts.push(value.to_string());
                index += 1;
            }
        }
    }

    let symptom = symptom_parts.join(" ").trim().to_string();
    if symptom.is_empty() {
        return Err(
            "usage: cos agent diagnose [--quick] [--domain <domain>] [--path <path>] <symptom>"
                .to_string(),
        );
    }
    let domain = explicit_domain.unwrap_or_else(|| infer_domain(&symptom));
    Ok(Options {
        symptom,
        domain,
        quick,
        path,
    })
}

fn infer_domain(symptom: &str) -> DiagnosticDomain {
    let text = symptom.to_lowercase();
    let groups: &[(DiagnosticDomain, &[&str])] = &[
        (
            DiagnosticDomain::Crash,
            &[
                "crash", "coredump", "segfault", "闪退", "崩溃", "oom", "killed", "被杀",
            ],
        ),
        (
            DiagnosticDomain::Network,
            &[
                "network", "internet", "wifi", "wi-fi", "dns", "latency", "packet", "网络", "网速",
                "断网", "延迟", "丢包", "联网",
            ],
        ),
        (
            DiagnosticDomain::Storage,
            &[
                "disk",
                "storage",
                "filesystem",
                "space",
                "磁盘",
                "硬盘",
                "空间",
                "容量",
                "文件系统",
            ],
        ),
        (
            DiagnosticDomain::Service,
            &[
                "service",
                "systemd",
                "daemon",
                "unit",
                "服务",
                "启动失败",
                "守护进程",
            ],
        ),
        (
            DiagnosticDomain::Thermal,
            &[
                "thermal",
                "temperature",
                "battery",
                "fan",
                "overheat",
                "温度",
                "过热",
                "风扇",
                "电池",
            ],
        ),
        (
            DiagnosticDomain::Security,
            &[
                "security", "login", "ssh", "sudo", "attack", "安全", "登录", "攻击", "入侵",
            ],
        ),
        (
            DiagnosticDomain::Performance,
            &[
                "slow",
                "performance",
                "cpu",
                "memory",
                "load",
                "freeze",
                "hang",
                "卡",
                "慢",
                "性能",
                "内存",
                "负载",
                "无响应",
            ],
        ),
    ];
    groups
        .iter()
        .find(|(_, words)| words.iter().any(|word| text.contains(word)))
        .map(|(domain, _)| *domain)
        .unwrap_or(DiagnosticDomain::General)
}

fn plan_for(options: &Options) -> Vec<ProbeSpec> {
    let mut probes = Vec::new();
    let mut ids = BTreeSet::new();
    let mut add = |probe: ProbeSpec| {
        if ids.insert(probe.id) {
            probes.push(probe);
        }
    };
    add(probe("info", "info", &[], "Operating-system identity"));
    add(probe("uptime", "uptime", &[], "System uptime"));

    match options.domain {
        DiagnosticDomain::General => {
            add(probe(
                "resources",
                "resources",
                &[],
                "Memory and workspace capacity",
            ));
            add(probe("load", "loadavg", &[], "System load"));
            add(probe(
                "failed-units",
                "failed_units",
                &[],
                "Failed systemd units",
            ));
            add(probe(
                "coredumps",
                "coredumps",
                &["--lines", "10"],
                "Recent crashes",
            ));
            add(probe(
                "updates",
                "pkg_updates",
                &[],
                "Pending package updates",
            ));
        }
        DiagnosticDomain::Performance => {
            add(probe(
                "resources",
                "resources",
                &[],
                "Memory and workspace capacity",
            ));
            add(probe("load", "loadavg", &[], "System load"));
            add(probe(
                "top-cpu",
                "top",
                &["--top", "8", "--by", "cpu", "--interval", "250"],
                "Top CPU consumers",
            ));
            add(probe(
                "top-memory",
                "top",
                &["--top", "8", "--by", "mem", "--interval", "250"],
                "Top memory consumers",
            ));
            add(probe("cgroup", "cgroup", &[], "Current cgroup limits"));
            add(probe(
                "sensors",
                "sensors",
                &[],
                "Thermal and power sensors",
            ));
            if !options.quick {
                add(probe(
                    "disk-rate",
                    "disk_io",
                    &["--interval", "250"],
                    "Live disk throughput",
                ));
            }
        }
        DiagnosticDomain::Network => {
            add(probe("network", "net", &[], "Interfaces and TCP sockets"));
            if !options.quick {
                add(probe(
                    "network-rate",
                    "net_rate",
                    &["--interval", "250"],
                    "Live interface throughput",
                ));
            }
            add(probe("load", "loadavg", &[], "System load"));
            add(probe(
                "failed-units",
                "failed_units",
                &[],
                "Failed network-related units",
            ));
        }
        DiagnosticDomain::Storage => {
            add(probe(
                "resources",
                "resources",
                &[],
                "Memory and workspace capacity",
            ));
            add(probe("mounts", "mounts", &[], "Mounted filesystems"));
            if !options.quick {
                add(probe(
                    "disk-rate",
                    "disk_io",
                    &["--interval", "250"],
                    "Live disk throughput",
                ));
            }
            if let Some(path) = options.path.as_deref() {
                add(probe(
                    "largest-files",
                    "largest_files",
                    &[path, "--top", "12", "--min-mb", "50"],
                    "Largest files on one filesystem",
                ));
            }
        }
        DiagnosticDomain::Service => {
            add(probe(
                "failed-units",
                "failed_units",
                &[],
                "Failed systemd units",
            ));
            add(probe(
                "journal-errors",
                "journal",
                &["--priority", "3", "--lines", "100"],
                "Recent high-priority journal entries",
            ));
            add(probe(
                "coredumps",
                "coredumps",
                &["--lines", "20"],
                "Recent crashes",
            ));
        }
        DiagnosticDomain::Crash => {
            add(probe(
                "coredumps",
                "coredumps",
                &["--lines", "20"],
                "Recent crashes",
            ));
            add(probe(
                "journal-errors",
                "journal",
                &["--priority", "3", "--lines", "100"],
                "Recent high-priority journal entries",
            ));
            add(probe(
                "kernel-log",
                "dmesg",
                &["--lines", "120"],
                "Recent kernel messages",
            ));
            add(probe(
                "resources",
                "resources",
                &[],
                "Memory and workspace capacity",
            ));
        }
        DiagnosticDomain::Thermal => {
            add(probe(
                "sensors",
                "sensors",
                &[],
                "Thermal and power sensors",
            ));
            add(probe("load", "loadavg", &[], "System load"));
            add(probe(
                "top-cpu",
                "top",
                &["--top", "8", "--by", "cpu", "--interval", "250"],
                "Top CPU consumers",
            ));
            add(probe(
                "resources",
                "resources",
                &[],
                "Memory and workspace capacity",
            ));
        }
        DiagnosticDomain::Security => {
            add(probe("sessions", "who", &[], "Logged-in sessions"));
            add(probe("network", "net", &[], "Interfaces and TCP sockets"));
            add(probe(
                "journal-errors",
                "journal",
                &["--priority", "4", "--lines", "120"],
                "Recent warning and error journal entries",
            ));
            add(probe(
                "kernel-log",
                "dmesg",
                &["--lines", "120"],
                "Recent kernel messages",
            ));
        }
    }
    probes
}

fn probe(
    id: &'static str,
    command: &'static str,
    args: &[&str],
    description: &'static str,
) -> ProbeSpec {
    ProbeSpec {
        id,
        command,
        args: args.iter().map(|arg| (*arg).to_string()).collect(),
        description,
    }
}

fn collect_evidence<F>(probes: &[ProbeSpec], runner: F) -> Vec<Evidence>
where
    F: Fn(&str, &[String]) -> Result<Value, String>,
{
    probes
        .iter()
        .map(|probe| {
            let started = Instant::now();
            match runner(probe.command, &probe.args) {
                Ok(data) => Evidence {
                    id: probe.id.to_string(),
                    command: format_command(probe),
                    description: probe.description.to_string(),
                    status: "ok",
                    latency_ms: started.elapsed().as_millis() as u64,
                    data: Some(data),
                    error: None,
                },
                Err(error) => Evidence {
                    id: probe.id.to_string(),
                    command: format_command(probe),
                    description: probe.description.to_string(),
                    status: "error",
                    latency_ms: started.elapsed().as_millis() as u64,
                    data: None,
                    error: Some(
                        crate::agent::safety::redact::Redactor::default_set().redact(&error),
                    ),
                },
            }
        })
        .collect()
}

fn format_command(probe: &ProbeSpec) -> String {
    if probe.args.is_empty() {
        format!("cos sys {}", probe.command)
    } else {
        format!("cos sys {} {}", probe.command, probe.args.join(" "))
    }
}

fn analyze(evidence: &[Evidence]) -> Vec<Finding> {
    let mut findings = Vec::new();
    if let Some(resources) = data(evidence, "resources") {
        if let (Some(total), Some(available)) = (
            resources
                .pointer("/memory/total_mb")
                .and_then(Value::as_u64),
            resources
                .pointer("/memory/available_mb")
                .and_then(Value::as_u64),
        ) {
            if total > 0 {
                let ratio = available as f64 / total as f64;
                if ratio < 0.1 {
                    findings.push(finding(
                        "memory-critical",
                        Severity::Critical,
                        "Very little memory is available",
                        format!("{available} MiB of {total} MiB is available."),
                        0.98,
                        &["resources"],
                        "Inspect top memory consumers and stop or restart the leaking workload.",
                    ));
                } else if ratio < 0.2 {
                    findings.push(finding(
                        "memory-low",
                        Severity::Warning,
                        "Available memory is low",
                        format!("{available} MiB of {total} MiB is available."),
                        0.95,
                        &["resources"],
                        "Inspect top memory consumers before starting additional workloads.",
                    ));
                }
            }
        }
        if let (Some(total), Some(free)) = (
            resources.pointer("/disk/total_mb").and_then(Value::as_u64),
            resources.pointer("/disk/free_mb").and_then(Value::as_u64),
        ) {
            if total > 0 {
                let ratio = free as f64 / total as f64;
                if ratio < 0.08 {
                    findings.push(finding(
                        "disk-critical",
                        Severity::Critical,
                        "Workspace filesystem is nearly full",
                        format!("{free} MiB of {total} MiB remains free."),
                        0.98,
                        &["resources"],
                        "Inspect the largest files and remove or archive unnecessary data.",
                    ));
                } else if ratio < 0.15 {
                    findings.push(finding(
                        "disk-low",
                        Severity::Warning,
                        "Workspace filesystem has limited free space",
                        format!("{free} MiB of {total} MiB remains free."),
                        0.95,
                        &["resources"],
                        "Inspect large files and package caches before the filesystem fills.",
                    ));
                }
            }
        }
    }

    if let Some(load) = data(evidence, "load") {
        if let Some(per_core) = load.get("load_per_core_1min").and_then(Value::as_f64) {
            if per_core >= 2.0 {
                findings.push(finding(
                    "load-critical",
                    Severity::Critical,
                    "System load is far above CPU capacity",
                    format!("One-minute load per core is {per_core:.2}."),
                    0.96,
                    &["load"],
                    "Inspect CPU consumers and blocked I/O before terminating any process.",
                ));
            } else if per_core >= 1.0 {
                findings.push(finding(
                    "load-high",
                    Severity::Warning,
                    "System load is above CPU capacity",
                    format!("One-minute load per core is {per_core:.2}."),
                    0.93,
                    &["load"],
                    "Inspect CPU consumers and disk throughput to distinguish compute from I/O pressure.",
                ));
            }
        }
    }

    if let Some(top) = data(evidence, "top-cpu") {
        if let Some(process) = top
            .get("processes")
            .and_then(Value::as_array)
            .and_then(|items| items.first())
        {
            let cpu = process
                .get("cpu_percent")
                .and_then(Value::as_f64)
                .unwrap_or(0.0);
            if cpu >= 90.0 {
                let name = process
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown");
                let pid = process.get("pid").and_then(Value::as_u64).unwrap_or(0);
                findings.push(finding(
                    "cpu-hot-process",
                    Severity::Warning,
                    "A process is consuming most of a CPU core",
                    format!("{name} (pid {pid}) is using {cpu:.1}% CPU."),
                    0.95,
                    &["top-cpu"],
                    "Inspect the process command, threads, and recent logs before signalling it.",
                ));
            }
        }
    }

    if let Some(cgroup) = data(evidence, "cgroup") {
        if let (Some(current), Some(maximum)) = (
            cgroup
                .pointer("/memory/current_bytes")
                .and_then(Value::as_u64),
            cgroup.pointer("/memory/max_bytes").and_then(Value::as_u64),
        ) {
            if maximum > 0 && current as f64 / maximum as f64 >= 0.9 {
                findings.push(finding(
                    "cgroup-memory-limit",
                    Severity::Warning,
                    "The current cgroup is close to its memory limit",
                    format!(
                        "Cgroup memory usage is {:.1}% of its configured limit.",
                        current as f64 / maximum as f64 * 100.0
                    ),
                    0.97,
                    &["cgroup"],
                    "Reduce workload memory or raise the cgroup limit after confirming host capacity.",
                ));
            }
        }
    }

    if count(evidence, "failed-units", "/count") > 0 {
        findings.push(finding(
            "failed-systemd-units",
            Severity::Warning,
            "One or more systemd units are failed",
            format!(
                "{} failed unit(s) were reported.",
                count(evidence, "failed-units", "/count")
            ),
            0.98,
            &["failed-units"],
            "Inspect each failed unit's status and journal before restarting it.",
        ));
    }

    if count(evidence, "coredumps", "/count") > 0 {
        findings.push(finding(
            "recent-coredumps",
            Severity::Warning,
            "Recent process crashes were found",
            format!(
                "{} coredump record(s) were returned.",
                count(evidence, "coredumps", "/count")
            ),
            0.93,
            &["coredumps"],
            "Correlate the newest coredump with journal and kernel timestamps.",
        ));
    }

    if count(evidence, "journal-errors", "/count") > 0 {
        findings.push(finding(
            "journal-errors",
            Severity::Warning,
            "Recent high-priority journal entries were found",
            format!(
                "{} warning or error journal entrie(s) were returned.",
                count(evidence, "journal-errors", "/count")
            ),
            0.86,
            &["journal-errors"],
            "Group journal entries by unit and timestamp before selecting a remediation.",
        ));
    }

    if let Some(sensors) = data(evidence, "sensors") {
        let mut max_temp = 0.0_f64;
        collect_temperatures(sensors, &mut max_temp);
        if max_temp >= 90.0 {
            findings.push(finding(
                "thermal-critical",
                Severity::Critical,
                "A temperature sensor is critically hot",
                format!("Maximum observed temperature is {max_temp:.1} °C."),
                0.97,
                &["sensors", "top-cpu"],
                "Reduce load immediately and verify cooling and fan operation.",
            ));
        } else if max_temp >= 80.0 {
            findings.push(finding(
                "thermal-high",
                Severity::Warning,
                "A temperature sensor is hot",
                format!("Maximum observed temperature is {max_temp:.1} °C."),
                0.94,
                &["sensors", "top-cpu"],
                "Inspect CPU consumers, power mode, airflow, and fan readings.",
            ));
        }
        if let Some(capacity) = sensors
            .pointer("/battery/0/capacity_percent")
            .and_then(Value::as_u64)
        {
            if capacity <= 10 {
                findings.push(finding(
                    "battery-low",
                    Severity::Warning,
                    "Battery charge is low",
                    format!("Battery capacity is {capacity}%."),
                    0.99,
                    &["sensors"],
                    "Connect external power or save work before starting long operations.",
                ));
            }
        }
    }

    if let Some(network) = data(evidence, "network") {
        let interfaces = network
            .get("interfaces")
            .and_then(Value::as_array)
            .map(Vec::len)
            .unwrap_or(0);
        if interfaces == 0 {
            findings.push(finding(
                "network-no-interfaces",
                Severity::Warning,
                "No network interfaces were observed",
                "The local interface inventory is empty.".to_string(),
                0.92,
                &["network"],
                "Check NetworkManager, device drivers, and link state.",
            ));
        }
    }

    if count(evidence, "updates", "/count") > 0 {
        findings.push(finding(
            "package-updates",
            Severity::Info,
            "Package updates are available",
            format!(
                "{} package update(s) were reported.",
                count(evidence, "updates", "/count")
            ),
            0.99,
            &["updates"],
            "Review updates and create a recovery point before applying system-wide changes.",
        ));
    }

    let mut kernel_text = String::new();
    if let Some(kernel_log) = data(evidence, "kernel-log") {
        collect_text(kernel_log, &mut kernel_text);
    }
    let kernel_text = kernel_text.to_ascii_lowercase();
    for (needle, code, title, recommendation) in [
        (
            "out of memory",
            "kernel-oom",
            "The kernel reported an out-of-memory event",
            "Identify the killed process and inspect memory growth before restarting it.",
        ),
        (
            "segfault",
            "kernel-segfault",
            "The kernel reported a segmentation fault",
            "Correlate the process and timestamp with coredump metadata.",
        ),
        (
            "i/o error",
            "kernel-io-error",
            "The kernel reported an I/O error",
            "Inspect disk health and filesystem state before writing more data.",
        ),
        (
            "apparmor=\"denied\"",
            "apparmor-denial",
            "AppArmor denied an operation",
            "Inspect the denied profile and requested resource before changing policy.",
        ),
    ] {
        if kernel_text.contains(needle) {
            findings.push(finding(
                code,
                Severity::Warning,
                title,
                format!("Kernel evidence contains `{needle}`."),
                0.88,
                &["kernel-log"],
                recommendation,
            ));
        }
    }

    findings
}

fn data<'a>(evidence: &'a [Evidence], id: &str) -> Option<&'a Value> {
    evidence
        .iter()
        .find(|item| item.id == id && item.status == "ok")
        .and_then(|item| item.data.as_ref())
}

fn count(evidence: &[Evidence], id: &str, pointer: &str) -> u64 {
    data(evidence, id)
        .and_then(|value| value.pointer(pointer))
        .and_then(Value::as_u64)
        .unwrap_or(0)
}

fn collect_temperatures(value: &Value, maximum: &mut f64) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                if matches!(key.as_str(), "temp_celsius" | "celsius") {
                    if let Some(temp) = child.as_f64() {
                        *maximum = maximum.max(temp);
                    }
                } else {
                    collect_temperatures(child, maximum);
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_temperatures(item, maximum);
            }
        }
        _ => {}
    }
}

fn collect_text(value: &Value, output: &mut String) {
    match value {
        Value::String(text) => {
            if !output.is_empty() {
                output.push('\n');
            }
            output.push_str(text);
        }
        Value::Object(map) => {
            for child in map.values() {
                collect_text(child, output);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_text(item, output);
            }
        }
        _ => {}
    }
}

fn finding(
    code: &'static str,
    severity: Severity,
    title: impl Into<String>,
    detail: impl Into<String>,
    confidence: f64,
    evidence: &[&str],
    recommendation: impl Into<String>,
) -> Finding {
    Finding {
        code,
        severity,
        title: title.into(),
        detail: detail.into(),
        confidence,
        evidence: evidence.iter().map(|id| (*id).to_string()).collect(),
        recommendation: recommendation.into(),
    }
}

fn round2(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_chinese_and_english_symptoms() {
        assert_eq!(infer_domain("网络为什么很慢"), DiagnosticDomain::Network);
        assert_eq!(
            infer_domain("the service keeps crashing"),
            DiagnosticDomain::Crash
        );
        assert_eq!(infer_domain("磁盘空间不够"), DiagnosticDomain::Storage);
        assert_eq!(
            infer_domain("computer is slow"),
            DiagnosticDomain::Performance
        );
    }

    #[test]
    fn explicit_domain_overrides_inference() {
        let options = parse_options(&[
            "--domain".into(),
            "service".into(),
            "the computer is slow".into(),
        ])
        .unwrap();
        assert_eq!(options.domain, DiagnosticDomain::Service);
    }

    #[test]
    fn quick_network_plan_skips_sampled_rate() {
        let options = Options {
            symptom: "network slow".into(),
            domain: DiagnosticDomain::Network,
            quick: true,
            path: None,
        };
        let ids = plan_for(&options)
            .into_iter()
            .map(|probe| probe.id)
            .collect::<Vec<_>>();
        assert!(ids.contains(&"network"));
        assert!(!ids.contains(&"network-rate"));
    }

    #[test]
    fn findings_reference_supporting_evidence() {
        let evidence = vec![
            ok(
                "resources",
                json!({"memory":{"total_mb":1000,"available_mb":50},"disk":{"total_mb":1000,"free_mb":50}}),
            ),
            ok("load", json!({"load_per_core_1min": 2.5})),
            ok("failed-units", json!({"count": 2})),
        ];
        let findings = analyze(&evidence);
        assert!(findings.iter().any(|item| item.code == "memory-critical"));
        assert!(findings.iter().any(|item| item.code == "disk-critical"));
        assert!(findings.iter().any(|item| item.code == "load-critical"));
        assert!(findings
            .iter()
            .find(|item| item.code == "failed-systemd-units")
            .unwrap()
            .evidence
            .contains(&"failed-units".to_string()));
    }

    #[test]
    fn report_surfaces_partial_probe_failure() {
        let args = vec!["computer slow".to_string()];
        let report = diagnose_with(&args, |command, _| {
            if command == "resources" {
                Err("permission denied: token sk-abcdefghijklmnopqrstuvwxyz".into())
            } else {
                Ok(json!({}))
            }
        })
        .unwrap();
        assert_eq!(report["status"], "warn");
        assert!(report["coverage"]["failed"].as_u64().unwrap() >= 1);
        let error = report["evidence"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["id"] == "resources")
            .unwrap()["error"]
            .as_str()
            .unwrap();
        assert!(!error.contains("sk-abcdefghijklmnopqrstuvwxyz"));
    }

    #[test]
    fn report_never_references_an_uncollected_probe() {
        let report = diagnose_with(&["general issue".into()], |command, _| match command {
            "loadavg" => Ok(json!({"load_per_core_1min": 3.0})),
            _ => Ok(json!({})),
        })
        .unwrap();
        let ids = report["evidence"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|item| item["id"].as_str())
            .collect::<BTreeSet<_>>();
        for finding in report["findings"].as_array().unwrap() {
            for evidence_id in finding["evidence"].as_array().unwrap() {
                assert!(ids.contains(evidence_id.as_str().unwrap()));
            }
        }

        #[test]
        fn detects_apparmor_denial_inside_raw_kernel_text() {
            let findings = analyze(&[ok(
                "kernel-log",
                json!({"entries": [{"raw": "audit: apparmor=\"DENIED\" operation=\"open\""}]}),
            )]);
            assert!(findings
                .iter()
                .any(|finding| finding.code == "apparmor-denial"));
        }
    }

    #[test]
    fn rejects_unknown_flags_and_missing_symptom() {
        assert!(parse_options(&["--bogus".into(), "x".into()]).is_err());
        assert!(parse_options(&[]).is_err());
    }

    fn ok(id: &str, data: Value) -> Evidence {
        Evidence {
            id: id.to_string(),
            command: id.to_string(),
            description: String::new(),
            status: "ok",
            latency_ms: 0,
            data: Some(data),
            error: None,
        }
    }
}
