use serde::Serialize;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::fs::File;
use std::io::Read;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::Path;
use std::str::FromStr;
use std::time::{Duration, Instant};
use tokio::net::TcpStream;

use crate::caps::{Cap, Scope, Verb};

use super::authority::Decision;
use super::protocol::BrokerError;
use super::wire::requests::NetworkDiagnose;

const DEFAULT_PORT: u16 = 443;
const DNS_TIMEOUT: Duration = Duration::from_secs(5);
const DIAGNOSE_ATTEMPTS: u8 = 3;
const DIAGNOSE_TIMEOUT_MS: u64 = 5_000;
const MAX_ATTEMPTS: u8 = 5;
const MAX_TIMEOUT_MS: u64 = 30_000;
const MAX_PROBE_BUDGET_MS: u64 = 20_000;
const MAX_RESOLVED_ADDRESSES: usize = 64;
const MAX_INTERFACES: usize = 256;
const MAX_ROUTE_BYTES: usize = 1024 * 1024;
const MAX_ROUTES: usize = 4096;

#[derive(Debug)]
enum Diagnostic {
    Interfaces,
    Routes,
    Dns(Target),
    Tcp {
        target: Target,
        attempts: u8,
        timeout_ms: u64,
    },
    Diagnose(Target),
}

impl Diagnostic {
    fn capabilities(&self) -> Vec<Cap> {
        match self {
            Self::Interfaces | Self::Routes => {
                vec![Cap::new(Verb::SYS_OBSERVE, Scope::name("network"))]
            }
            Self::Dns(target) => {
                vec![Cap::new(
                    Verb::NET_RESOLVE,
                    Scope::host(target.scope.clone()),
                )]
            }
            Self::Tcp { target, .. } => vec![
                Cap::new(Verb::NET_RESOLVE, Scope::host(target.scope.clone())),
                Cap::new(Verb::NET_PROBE, Scope::host(target.scope.clone())),
            ],
            Self::Diagnose(target) => vec![
                Cap::new(Verb::SYS_OBSERVE, Scope::name("network")),
                Cap::new(Verb::NET_RESOLVE, Scope::host(target.scope.clone())),
                Cap::new(Verb::NET_PROBE, Scope::host(target.scope.clone())),
            ],
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Target {
    scope: String,
    display: String,
    host: String,
    port: u16,
}

#[derive(Clone, Debug, Serialize)]
struct InterfaceRecord {
    name: String,
    operstate: Option<String>,
    carrier: Option<bool>,
    mtu: Option<u64>,
    address: Option<String>,
    speed_mbps: Option<i64>,
    rx_bytes: Option<u64>,
    tx_bytes: Option<u64>,
    rx_errors: Option<u64>,
    tx_errors: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
struct RouteRecord {
    interface: String,
    destination: String,
    gateway: String,
    mask: String,
    default: bool,
    metric: u64,
}

#[derive(Debug)]
struct Resolution {
    target: Target,
    latency_ms: f64,
    addresses: Vec<SocketAddr>,
    error: Option<String>,
}

pub async fn diagnose(params: Value, authority: &Decision) -> Result<Value, BrokerError> {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (params, authority);
        return Err(BrokerError::unavailable(
            "network diagnostics require Linux",
        ));
    }

    #[cfg(target_os = "linux")]
    {
        let request: NetworkDiagnose = serde_json::from_value(params).map_err(|error| {
            BrokerError::execution(format!("invalid network diagnostic request: {error}"))
        })?;
        let diagnostic = prepare(&request).map_err(BrokerError::execution)?;
        authority
            .require_app("netdiag")
            .map_err(BrokerError::authorization)?;
        let _authorized = authority
            .require_all(&diagnostic.capabilities())
            .map_err(BrokerError::authorization)?;

        match diagnostic {
            Diagnostic::Interfaces => interfaces().map_err(BrokerError::execution),
            Diagnostic::Routes => routes().map_err(BrokerError::execution),
            Diagnostic::Dns(target) => Ok(resolution_value(&resolve(&target).await)),
            Diagnostic::Tcp {
                target,
                attempts,
                timeout_ms,
            } => Ok(tcp(target, attempts, timeout_ms).await),
            Diagnostic::Diagnose(target) => {
                diagnose_all(target).await.map_err(BrokerError::execution)
            }
        }
    }
}

fn prepare(request: &NetworkDiagnose) -> Result<Diagnostic, String> {
    match request.action.as_str() {
        "interfaces" => {
            reject_fields(request, false, false)?;
            Ok(Diagnostic::Interfaces)
        }
        "routes" => {
            reject_fields(request, false, false)?;
            Ok(Diagnostic::Routes)
        }
        "dns" => {
            reject_fields(request, true, false)?;
            Ok(Diagnostic::Dns(parse_target(
                required_target(request)?,
                false,
            )?))
        }
        "tcp" => {
            reject_fields(request, true, true)?;
            let attempts = request
                .attempts
                .ok_or_else(|| "tcp requires attempts".to_string())?;
            let timeout_ms = request
                .timeout_ms
                .ok_or_else(|| "tcp requires timeout_ms".to_string())?;
            validate_probe_options(attempts, timeout_ms)?;
            Ok(Diagnostic::Tcp {
                target: parse_target(required_target(request)?, true)?,
                attempts,
                timeout_ms,
            })
        }
        "diagnose" => {
            reject_fields(request, true, false)?;
            Ok(Diagnostic::Diagnose(parse_target(
                required_target(request)?,
                true,
            )?))
        }
        other => Err(format!("unsupported network diagnostic action: {other}")),
    }
}

fn reject_fields(
    request: &NetworkDiagnose,
    target_allowed: bool,
    probe_options_allowed: bool,
) -> Result<(), String> {
    if !target_allowed && request.target.is_some() {
        return Err(format!(
            "{} does not accept target",
            request.action.as_str()
        ));
    }
    if !probe_options_allowed && (request.attempts.is_some() || request.timeout_ms.is_some()) {
        return Err(format!(
            "{} does not accept attempts or timeout_ms",
            request.action.as_str()
        ));
    }
    Ok(())
}

fn required_target(request: &NetworkDiagnose) -> Result<&str, String> {
    request
        .target
        .as_ref()
        .map(|target| target.as_str())
        .ok_or_else(|| format!("{} requires target", request.action.as_str()))
}

fn validate_probe_options(attempts: u8, timeout_ms: u64) -> Result<(), String> {
    if !(1..=MAX_ATTEMPTS).contains(&attempts) {
        return Err(format!("attempts must be between 1 and {MAX_ATTEMPTS}"));
    }
    if !(100..=MAX_TIMEOUT_MS).contains(&timeout_ms) {
        return Err(format!(
            "timeout_ms must be between 100 and {MAX_TIMEOUT_MS}"
        ));
    }
    if u64::from(attempts).saturating_mul(timeout_ms) > MAX_PROBE_BUDGET_MS {
        return Err(format!(
            "attempts multiplied by timeout_ms must not exceed {MAX_PROBE_BUDGET_MS}"
        ));
    }
    Ok(())
}

fn parse_target(raw: &str, require_port: bool) -> Result<Target, String> {
    if raw.is_empty()
        || raw.trim() != raw
        || raw.starts_with('-')
        || raw.contains("://")
        || raw
            .chars()
            .any(|character| character.is_control() || character.is_whitespace())
        || raw.contains(['/', '\\', '@', '#', '?'])
    {
        return Err("target must be a host or host:port".to_string());
    }

    let (host, port, explicit_port, bracketed) = if let Some(rest) = raw.strip_prefix('[') {
        let end = rest
            .find(']')
            .ok_or_else(|| "target has an invalid bracketed IPv6 address".to_string())?;
        let host = &rest[..end];
        let suffix = &rest[end + 1..];
        let port = if suffix.is_empty() {
            None
        } else {
            Some(parse_port(suffix.strip_prefix(':').ok_or_else(|| {
                "target has an invalid bracketed IPv6 port".to_string()
            })?)?)
        };
        (host, port, port.is_some(), true)
    } else if IpAddr::from_str(raw).is_ok() {
        (raw, None, false, false)
    } else if let Some((host, port)) = raw.rsplit_once(':') {
        (host, Some(parse_port(port)?), true, false)
    } else {
        (raw, None, false, false)
    };

    if require_port && !explicit_port {
        return Err("TCP diagnostics require an explicit host:port target".to_string());
    }
    if host.is_empty() {
        return Err("target host is empty".to_string());
    }

    let canonical_host = match IpAddr::from_str(host) {
        Ok(IpAddr::V4(address)) if !bracketed => address.to_string(),
        Ok(IpAddr::V6(address)) if bracketed || !explicit_port => address.to_string(),
        Ok(_) if bracketed => {
            return Err("brackets are only valid around an IPv6 address".to_string())
        }
        Ok(_) => return Err("IPv6 targets with a port must use brackets".to_string()),
        Err(_) if bracketed => {
            return Err("brackets are only valid around an IPv6 address".to_string())
        }
        Err(_) => canonical_domain(host)?,
    };
    let port = port.unwrap_or(DEFAULT_PORT);
    let display = if explicit_port {
        format_endpoint(&canonical_host, port)
    } else {
        canonical_host.clone()
    };
    Ok(Target {
        scope: raw.to_string(),
        display,
        host: canonical_host,
        port,
    })
}

fn parse_port(raw: &str) -> Result<u16, String> {
    if raw.is_empty() || !raw.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err("target port must be an integer".to_string());
    }
    let port = raw
        .parse::<u16>()
        .map_err(|_| "target port is out of range".to_string())?;
    if port == 0 {
        return Err("target port is out of range".to_string());
    }
    Ok(port)
}

fn canonical_domain(raw: &str) -> Result<String, String> {
    let raw = raw
        .strip_suffix('.')
        .filter(|value| !value.ends_with('.'))
        .unwrap_or(raw);
    let domain =
        match url::Host::parse(raw).map_err(|_| "target hostname is invalid".to_string())? {
            url::Host::Domain(domain) => domain,
            url::Host::Ipv4(address) => return Ok(address.to_string()),
            url::Host::Ipv6(address) => return Ok(address.to_string()),
        };
    if domain.is_empty()
        || domain.len() > 253
        || domain.split('.').any(|label| {
            label.is_empty()
                || label.len() > 63
                || label.starts_with('-')
                || label.ends_with('-')
                || !label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
    {
        return Err("target hostname is invalid".to_string());
    }
    Ok(domain.to_ascii_lowercase())
}

fn format_endpoint(host: &str, port: u16) -> String {
    if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

async fn resolve(target: &Target) -> Resolution {
    let started = Instant::now();
    let lookup = tokio::time::timeout(
        DNS_TIMEOUT,
        tokio::net::lookup_host((target.host.as_str(), target.port)),
    )
    .await;
    let addresses = match lookup {
        Err(_) => {
            return Resolution {
                target: target.clone(),
                latency_ms: elapsed_ms(started),
                addresses: Vec::new(),
                error: Some(format!(
                    "DNS resolution exceeded {}s",
                    DNS_TIMEOUT.as_secs()
                )),
            };
        }
        Ok(Err(error)) => {
            return Resolution {
                target: target.clone(),
                latency_ms: elapsed_ms(started),
                addresses: Vec::new(),
                error: Some(error.to_string()),
            };
        }
        Ok(Ok(addresses)) => addresses,
    };

    let addresses = match bounded_unique_addresses(addresses) {
        Ok(addresses) => addresses,
        Err(error) => {
            return Resolution {
                target: target.clone(),
                latency_ms: elapsed_ms(started),
                addresses: Vec::new(),
                error: Some(error),
            };
        }
    };
    let error = addresses
        .is_empty()
        .then(|| "DNS resolution returned no TCP addresses".to_string());
    Resolution {
        target: target.clone(),
        latency_ms: elapsed_ms(started),
        addresses,
        error,
    }
}

fn bounded_unique_addresses(
    addresses: impl IntoIterator<Item = SocketAddr>,
) -> Result<Vec<SocketAddr>, String> {
    let mut seen = HashSet::new();
    let mut unique = Vec::new();
    for address in addresses {
        if !seen.insert(address) {
            continue;
        }
        if unique.len() == MAX_RESOLVED_ADDRESSES {
            return Err(format!(
                "DNS resolution returned more than {MAX_RESOLVED_ADDRESSES} addresses"
            ));
        }
        unique.push(address);
    }
    Ok(unique)
}

fn resolution_value(resolution: &Resolution) -> Value {
    let addresses = resolution
        .addresses
        .iter()
        .map(|address| {
            json!({
                "ip": address.ip().to_string(),
                "family": if address.is_ipv6() { "ipv6" } else { "ipv4" },
                "canonical_name": Value::Null,
            })
        })
        .collect::<Vec<_>>();
    let mut value = json!({
        "target": resolution.target.display,
        "host": resolution.target.host,
        "port": resolution.target.port,
        "resolved": !resolution.addresses.is_empty(),
        "latency_ms": resolution.latency_ms,
        "addresses": addresses,
    });
    if let Some(error) = &resolution.error {
        value["error"] = Value::String(error.clone());
    }
    value
}

async fn tcp(target: Target, attempts: u8, timeout_ms: u64) -> Value {
    let resolution = resolve(&target).await;
    let mut value = resolution_value(&resolution);
    value["dns_latency_ms"] = value["latency_ms"].take();
    value["latency_ms"] = json!({"min": null, "max": null, "average": null});
    value["reachable"] = Value::Bool(false);
    value["attempts"] = Value::Array(Vec::new());
    value["success_count"] = json!(0);
    value["failure_count"] = json!(0);
    if resolution.addresses.is_empty() {
        return value;
    }

    let timeout = Duration::from_millis(timeout_ms);
    let mut samples = Vec::with_capacity(usize::from(attempts));
    let mut latencies = Vec::new();
    for index in 0..attempts {
        let address = resolution.addresses[usize::from(index) % resolution.addresses.len()];
        let (sample, latency) = connect(address, timeout, index + 1).await;
        if let Some(latency) = latency {
            latencies.push(latency);
        }
        samples.push(sample);
    }
    let success_count = latencies.len();
    value["reachable"] = Value::Bool(success_count > 0);
    value["attempts"] = Value::Array(samples);
    value["success_count"] = json!(success_count);
    value["failure_count"] = json!(usize::from(attempts) - success_count);
    value["latency_ms"] = if latencies.is_empty() {
        json!({"min": null, "max": null, "average": null})
    } else {
        let minimum = latencies.iter().copied().fold(f64::INFINITY, f64::min);
        let maximum = latencies.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        let average = latencies.iter().sum::<f64>() / latencies.len() as f64;
        json!({
            "min": round_ms(minimum),
            "max": round_ms(maximum),
            "average": round_ms(average),
        })
    };
    value
}

async fn connect(address: SocketAddr, timeout: Duration, attempt: u8) -> (Value, Option<f64>) {
    let started = Instant::now();
    match tokio::time::timeout(timeout, TcpStream::connect(address)).await {
        Ok(Ok(stream)) => {
            drop(stream);
            let latency = elapsed_ms(started);
            (
                json!({
                    "attempt": attempt,
                    "ok": true,
                    "ip": address.ip().to_string(),
                    "latency_ms": latency,
                }),
                Some(latency),
            )
        }
        Ok(Err(error)) => (
            json!({
                "attempt": attempt,
                "ok": false,
                "ip": address.ip().to_string(),
                "error": error.to_string(),
            }),
            None,
        ),
        Err(_) => (
            json!({
                "attempt": attempt,
                "ok": false,
                "ip": address.ip().to_string(),
                "error": format!("TCP connection exceeded {}ms", timeout.as_millis()),
            }),
            None,
        ),
    }
}

fn interfaces() -> Result<Value, String> {
    inspect_interfaces().map(|interfaces| interfaces_value(&interfaces))
}

fn inspect_interfaces() -> Result<Vec<InterfaceRecord>, String> {
    let root = Path::new("/sys/class/net");
    let mut entries = std::fs::read_dir(root)
        .map_err(|error| format!("read {}: {error}", root.display()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("read {} entry: {error}", root.display()))?;
    entries.sort_by_key(|entry| entry.file_name());
    if entries.len() > MAX_INTERFACES {
        return Err(format!("network interface count exceeds {MAX_INTERFACES}"));
    }

    let mut interfaces = Vec::with_capacity(entries.len());
    for entry in entries {
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| "network interface name is not UTF-8".to_string())?;
        let path = entry.path();
        interfaces.push(InterfaceRecord {
            name,
            operstate: read_optional(&path.join("operstate")),
            carrier: read_optional(&path.join("carrier")).map(|value| value == "1"),
            mtu: read_optional_u64(&path.join("mtu")),
            address: read_optional(&path.join("address")),
            speed_mbps: read_optional_i64(&path.join("speed")),
            rx_bytes: read_optional_u64(&path.join("statistics/rx_bytes")),
            tx_bytes: read_optional_u64(&path.join("statistics/tx_bytes")),
            rx_errors: read_optional_u64(&path.join("statistics/rx_errors")),
            tx_errors: read_optional_u64(&path.join("statistics/tx_errors")),
        });
    }
    Ok(interfaces)
}

fn inspect_routes() -> Result<Vec<RouteRecord>, String> {
    let contents = read_bounded(Path::new("/proc/net/route"), MAX_ROUTE_BYTES)?;
    parse_routes(&contents)
}

fn routes() -> Result<Value, String> {
    inspect_routes().map(|routes| routes_value(&routes))
}

fn interfaces_value(interfaces: &[InterfaceRecord]) -> Value {
    json!({
        "interfaces": interfaces,
        "count": interfaces.len(),
    })
}

fn routes_value(routes: &[RouteRecord]) -> Value {
    let default_routes = routes
        .iter()
        .filter(|route| route.default)
        .collect::<Vec<_>>();
    json!({
        "routes": routes,
        "count": routes.len(),
        "default_routes": default_routes,
    })
}

fn parse_routes(contents: &str) -> Result<Vec<RouteRecord>, String> {
    let mut lines = contents.lines();
    let header = lines
        .next()
        .ok_or_else(|| "network route table is missing its header".to_string())?;
    let header_fields = header.split_whitespace().collect::<Vec<_>>();
    if header_fields.first() != Some(&"Iface")
        || header_fields.get(1) != Some(&"Destination")
        || header_fields.get(2) != Some(&"Gateway")
        || header_fields.get(6) != Some(&"Metric")
        || header_fields.get(7) != Some(&"Mask")
    {
        return Err("network route table header is invalid".to_string());
    }

    let mut routes = Vec::new();
    for (index, line) in lines.enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        if routes.len() == MAX_ROUTES {
            return Err(format!("network route count exceeds {MAX_ROUTES}"));
        }
        let fields = line.split_whitespace().collect::<Vec<_>>();
        if fields.len() < 11 {
            return Err(format!("malformed network route row {}", index + 2));
        }
        routes.push(RouteRecord {
            interface: fields[0].to_string(),
            destination: decode_ipv4(fields[1])?,
            gateway: decode_ipv4(fields[2])?,
            mask: decode_ipv4(fields[7])?,
            default: fields[1] == "00000000" && fields[7] == "00000000",
            metric: fields[6]
                .parse::<u64>()
                .map_err(|_| format!("invalid route metric on row {}", index + 2))?,
        });
    }
    Ok(routes)
}

fn decode_ipv4(raw: &str) -> Result<String, String> {
    u32::from_str_radix(raw, 16)
        .map(|value| Ipv4Addr::from(value.to_le_bytes()).to_string())
        .map_err(|_| "invalid IPv4 route field".to_string())
}

fn read_bounded(path: &Path, limit: usize) -> Result<String, String> {
    let mut file = File::open(path).map_err(|error| format!("open {}: {error}", path.display()))?;
    let mut bytes = Vec::new();
    file.by_ref()
        .take(limit.saturating_add(1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("read {}: {error}", path.display()))?;
    if bytes.len() > limit {
        return Err(format!("{} exceeds {limit} bytes", path.display()));
    }
    String::from_utf8(bytes).map_err(|_| format!("{} is not UTF-8", path.display()))
}

fn read_optional(path: &Path) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|value| value.trim().to_string())
}

fn read_optional_u64(path: &Path) -> Option<u64> {
    read_optional(path)?.parse().ok()
}

fn read_optional_i64(path: &Path) -> Option<i64> {
    read_optional(path)?.parse().ok()
}

async fn diagnose_all(target: Target) -> Result<Value, String> {
    let interface_records = inspect_interfaces()?;
    let route_records = inspect_routes()?;
    let tcp = tcp(target.clone(), DIAGNOSE_ATTEMPTS, DIAGNOSE_TIMEOUT_MS).await;

    let has_non_loopback = interface_records
        .iter()
        .any(|interface| interface.name != "lo");
    let has_up_interface = interface_records
        .iter()
        .any(|interface| interface.name != "lo" && interface.operstate.as_deref() == Some("up"));
    let has_default_route = route_records.iter().any(|route| route.default);

    let mut findings = Vec::new();
    if !has_non_loopback {
        findings.push(json!({
            "stage": "link",
            "severity": "critical",
            "message": "No non-loopback network interface is present.",
        }));
    } else if !has_up_interface {
        findings.push(json!({
            "stage": "link",
            "severity": "critical",
            "message": "No non-loopback interface reports operstate=up.",
        }));
    }
    if !has_default_route {
        findings.push(json!({
            "stage": "route",
            "severity": "critical",
            "message": "No IPv4 default route is installed.",
        }));
    }
    if tcp["resolved"].as_bool() == Some(false) {
        findings.push(json!({
            "stage": "dns",
            "severity": "critical",
            "message": tcp["error"]
                .as_str()
                .unwrap_or("DNS resolution failed."),
        }));
    } else if tcp["reachable"].as_bool() == Some(false) {
        findings.push(json!({
            "stage": "tcp",
            "severity": "warning",
            "message": "DNS succeeded but the TCP target was unreachable.",
        }));
    }
    if findings.is_empty() {
        findings.push(json!({
            "stage": "tcp",
            "severity": "info",
            "message": "Local link, default route, DNS, and TCP reachability succeeded.",
        }));
    }
    let status = if findings
        .iter()
        .any(|finding| finding["severity"] == "critical")
    {
        "critical"
    } else if findings
        .iter()
        .any(|finding| finding["severity"] == "warning")
    {
        "warn"
    } else {
        "ok"
    };
    Ok(json!({
        "status": status,
        "target": target.display,
        "findings": findings,
        "interfaces": interfaces_value(&interface_records),
        "routes": routes_value(&route_records),
        "tcp": tcp,
    }))
}

fn elapsed_ms(started: Instant) -> f64 {
    round_ms(started.elapsed().as_secs_f64() * 1000.0)
}

fn round_ms(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/network_diagnostics.rs"
    ));
}
