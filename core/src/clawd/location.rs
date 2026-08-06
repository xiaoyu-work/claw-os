use serde_json::{json, Value};
use std::fs;
use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::os::unix::io::AsRawFd;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};
use zbus::blocking::{Connection, Proxy};
use zbus::zvariant::OwnedObjectPath;

use crate::caps::{Cap, CapSet, Scope, Verb};

use super::client_identity::ClientIdentity;

const HELPER_TIMEOUT: Duration = Duration::from_secs(35);
const LOCATION_WAIT: Duration = Duration::from_secs(25);
const STREAM_CAP_BYTES: usize = 1024 * 1024;
const GEOCLUE_SERVICE: &str = "org.freedesktop.GeoClue2";
const GEOCLUE_MANAGER_PATH: &str = "/org/freedesktop/GeoClue2/Manager";
const GEOCLUE_MANAGER_INTERFACE: &str = "org.freedesktop.GeoClue2.Manager";
const GEOCLUE_CLIENT_INTERFACE: &str = "org.freedesktop.GeoClue2.Client";
const GEOCLUE_LOCATION_INTERFACE: &str = "org.freedesktop.GeoClue2.Location";
const GEOCLUE_DESKTOP_ID: &str = "com.clawos.Agent";

pub async fn query(params: Value, client: &ClientIdentity) -> Result<Value, String> {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (params, client);
        return Err("Location Manager requires Linux GeoClue".to_string());
    }

    #[cfg(target_os = "linux")]
    {
        if unsafe { libc::geteuid() } != 0 {
            return Err("Location Manager requires root clawd".to_string());
        }
        let uid = client.require_uid()?;
        if uid == 0 {
            return Err(
                "Location Manager requires a non-root desktop user session".to_string(),
            );
        }
        let gid = client
            .gid
            .ok_or_else(|| "clawd peer gid is unavailable".to_string())?;
        let home = client.require_home_dir()?;
        let peer_pid = client
            .pid
            .ok_or_else(|| "clawd peer pid is unavailable".to_string())?;
        let session_id = required_string(&params, "session")?;
        let action = required_string(&params, "action")?;
        let accuracy = optional_string(&params, "accuracy")?.unwrap_or_else(|| "city".to_string());
        validate_action(&action, &accuracy)?;
        crate::paths::with_user_override(uid, home.clone(), async {
            authorize_session(
                &session_id,
                peer_pid,
                Cap::new(Verb::DEVICE_LOCATION, Scope::Wild),
            )
        })
        .await?;

        let environment = UserEnvironment::new(uid, gid, home)?;
        let location = run_helper(&environment, accuracy_level(&accuracy)?).await?;
        let latitude = location["latitude"]
            .as_f64()
            .ok_or_else(|| "GeoClue helper omitted latitude".to_string())?;
        let longitude = location["longitude"]
            .as_f64()
            .ok_or_else(|| "GeoClue helper omitted longitude".to_string())?;
        match action.as_str() {
            "locate" => Ok(json!({
                "action": action,
                "provider": "geoclue2",
                "requested_accuracy": accuracy,
                "location": location,
            })),
            "timezone" => Ok(json!({
                "action": action,
                "provider": "geoclue2",
                "requested_accuracy": accuracy,
                "location": location,
                "timezone": timezone_suggestions(latitude, longitude)?,
            })),
            _ => unreachable!("validated location action"),
        }
    }
}

fn authorize_session(session_id: &str, peer_pid: u32, requested: Cap) -> Result<(), String> {
    let session = crate::proc::session_info_by_id(session_id)
        .ok_or_else(|| format!("location-manager session not found: {session_id}"))?;
    if session.app_id.as_deref() != Some("location-manager") {
        return Err("location access is restricted to the location-manager App".to_string());
    }
    if session.pending_bind || session.pid == 0 {
        return Err("location-manager session is not bound to a process".to_string());
    }
    let expected_start = session
        .start_time_ticks
        .ok_or_else(|| "location-manager session has no process identity".to_string())?;
    if crate::proc::read_start_time_ticks_pub(session.pid) != Some(expected_start) {
        return Err("location-manager session process identity is stale".to_string());
    }
    if !crate::proc::process_descends_from(peer_pid, session.pid) {
        return Err("location request did not originate from the authorized session".to_string());
    }
    let mut caps = session.caps.unwrap_or_else(CapSet::new);
    if let Some(transient) = session.transient_caps {
        caps.extend(transient.iter().cloned());
    }
    if !caps.covers(&requested) {
        return Err(format!(
            "location-manager session lacks {}",
            requested.verb.as_str()
        ));
    }
    Ok(())
}

pub fn helper(args: &[String]) -> Result<Value, String> {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = args;
        return Err("GeoClue helper requires Linux".to_string());
    }

    #[cfg(target_os = "linux")]
    {
        if unsafe { libc::geteuid() } == 0 {
            return Err("GeoClue helper must run as the requesting user".to_string());
        }
        if unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) } != 0 {
            return Err(format!(
                "harden GeoClue helper: {}",
                std::io::Error::last_os_error()
            ));
        }
        if args.len() != 2 {
            return Err("GeoClue helper requires an accuracy level and broker fd".to_string());
        }
        let accuracy = args[0]
            .parse::<u32>()
            .map_err(|_| "invalid GeoClue accuracy level".to_string())?;
        if !matches!(accuracy, 1 | 4 | 5 | 6 | 8) {
            return Err("unsupported GeoClue accuracy level".to_string());
        }
        let broker_fd = args[1]
            .parse::<libc::c_int>()
            .map_err(|_| "invalid GeoClue broker fd".to_string())?;
        validate_helper_parent(broker_fd)?;

        let connection =
            Connection::system().map_err(|error| format!("connect to system D-Bus: {error}"))?;
        let manager = Proxy::new(
            &connection,
            GEOCLUE_SERVICE,
            GEOCLUE_MANAGER_PATH,
            GEOCLUE_MANAGER_INTERFACE,
        )
        .map_err(|error| format!("connect to GeoClue manager: {error}"))?;
        let client_path: OwnedObjectPath = manager
            .call("GetClient", &())
            .map_err(|error| format!("create GeoClue client: {error}"))?;
        let client = Proxy::new(
            &connection,
            GEOCLUE_SERVICE,
            client_path.as_str(),
            GEOCLUE_CLIENT_INTERFACE,
        )
        .map_err(|error| format!("connect to GeoClue client: {error}"))?;
        client
            .set_property("DesktopId", GEOCLUE_DESKTOP_ID)
            .map_err(|error| format!("set GeoClue DesktopId: {error}"))?;
        client
            .set_property("RequestedAccuracyLevel", accuracy)
            .map_err(|error| format!("set GeoClue accuracy: {error}"))?;
        client
            .set_property("DistanceThreshold", 0_u32)
            .map_err(|error| format!("set GeoClue distance threshold: {error}"))?;
        client
            .set_property("TimeThreshold", 0_u32)
            .map_err(|error| format!("set GeoClue time threshold: {error}"))?;
        let _: () = client
            .call("Start", &())
            .map_err(|error| format!("start GeoClue client: {error}"))?;

        let result = read_location(&connection, &client, accuracy);
        let stop: Result<(), String> = client
            .call::<_, _, ()>("Stop", &())
            .map_err(|error| format!("stop GeoClue client: {error}"));
        match (result, stop) {
            (Ok(value), Ok(())) => Ok(value),
            (Ok(_), Err(error)) => Err(error),
            (Err(error), Ok(())) => Err(error),
            (Err(error), Err(stop_error)) => Err(format!(
                "{error}; GeoClue cleanup also failed: {stop_error}"
            )),
        }
    }
}

fn read_location(
    connection: &Connection,
    client: &Proxy<'_>,
    requested_accuracy: u32,
) -> Result<Value, String> {
    let deadline = Instant::now() + LOCATION_WAIT;
    let location_path = loop {
        let path: OwnedObjectPath = client
            .get_property("Location")
            .map_err(|error| format!("read GeoClue location path: {error}"))?;
        if path.as_str() != "/" {
            break path;
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "GeoClue did not provide a location within {}s",
                LOCATION_WAIT.as_secs()
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let location = Proxy::new(
        connection,
        GEOCLUE_SERVICE,
        location_path.as_str(),
        GEOCLUE_LOCATION_INTERFACE,
    )
    .map_err(|error| format!("connect to GeoClue location: {error}"))?;
    let latitude: f64 = location
        .get_property("Latitude")
        .map_err(|error| format!("read GeoClue latitude: {error}"))?;
    let longitude: f64 = location
        .get_property("Longitude")
        .map_err(|error| format!("read GeoClue longitude: {error}"))?;
    let accuracy: f64 = location
        .get_property("Accuracy")
        .map_err(|error| format!("read GeoClue accuracy: {error}"))?;
    if !latitude.is_finite()
        || !longitude.is_finite()
        || !accuracy.is_finite()
        || !(-90.0..=90.0).contains(&latitude)
        || !(-180.0..=180.0).contains(&longitude)
        || accuracy < 0.0
    {
        return Err("GeoClue returned invalid coordinates or accuracy".to_string());
    }
    let altitude = optional_f64_property(&location, "Altitude")?;
    let speed = optional_f64_property(&location, "Speed")?.filter(|value| *value >= 0.0);
    let heading = optional_f64_property(&location, "Heading")?.filter(|value| *value >= 0.0);
    let description = location
        .get_property::<String>("Description")
        .map_err(|error| format!("read GeoClue description: {error}"))?
        .trim()
        .to_string();
    let description = (!description.is_empty()).then_some(description);
    let timestamp = location
        .get_property::<(u64, u64)>("Timestamp")
        .map_err(|error| format!("read GeoClue timestamp: {error}"))?;
    let timestamp = (timestamp.0 != 0).then_some(timestamp);
    Ok(json!({
        "latitude": latitude,
        "longitude": longitude,
        "accuracy_m": accuracy,
        "altitude_m": altitude,
        "speed_m_s": speed,
        "heading_degrees": heading,
        "description": description,
        "timestamp": timestamp.map(|(seconds, microseconds)| json!({
            "unix_seconds": seconds,
            "microseconds": microseconds,
        })),
        "requested_accuracy_level": requested_accuracy,
        "received_at": chrono::Utc::now().to_rfc3339(),
    }))
}

fn optional_f64_property(proxy: &Proxy<'_>, name: &str) -> Result<Option<f64>, String> {
    let value = proxy
        .get_property::<f64>(name)
        .map_err(|error| format!("read GeoClue {name}: {error}"))?;
    Ok((value.is_finite() && value > -1.0e300).then_some(value))
}

fn validate_action(action: &str, accuracy: &str) -> Result<(), String> {
    if !matches!(action, "locate" | "timezone") {
        return Err(format!("unknown location action: {action}"));
    }
    accuracy_level(accuracy).map(|_| ())
}

fn accuracy_level(accuracy: &str) -> Result<u32, String> {
    match accuracy {
        "country" => Ok(1),
        "city" => Ok(4),
        "neighborhood" => Ok(5),
        "street" => Ok(6),
        "exact" => Ok(8),
        _ => Err("accuracy must be country|city|neighborhood|street|exact".to_string()),
    }
}

async fn run_helper(environment: &UserEnvironment, accuracy: u32) -> Result<Value, String> {
    let output = run_user_command(
        PathBuf::from("/proc/self/exe"),
        vec!["--location-helper".to_string(), accuracy.to_string()],
        environment.clone(),
        HELPER_TIMEOUT,
    )
    .await?;
    if !output.status.success() {
        return Err(format!(
            "GeoClue helper exited {}: {}",
            output.status.code().unwrap_or(-1),
            tail(&output.stderr)
        ));
    }
    serde_json::from_str(output.stdout.trim())
        .map_err(|error| format!("parse GeoClue helper JSON: {error}"))
}

fn timezone_suggestions(latitude: f64, longitude: f64) -> Result<Value, String> {
    let table = [
        "/usr/share/zoneinfo/zone1970.tab",
        "/usr/share/zoneinfo/zone.tab",
    ]
    .into_iter()
    .find(|path| Path::new(path).is_file())
    .ok_or_else(|| "timezone coordinate table is unavailable".to_string())?;
    let data =
        fs::read_to_string(table).map_err(|error| format!("read timezone table: {error}"))?;
    let mut candidates = Vec::new();
    for line in data.lines() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let fields = line.split('\t').collect::<Vec<_>>();
        if fields.len() < 3 {
            continue;
        }
        let Some((zone_latitude, zone_longitude)) = parse_iso6709(fields[1]) else {
            continue;
        };
        candidates.push((
            haversine_km(latitude, longitude, zone_latitude, zone_longitude),
            fields[2].to_string(),
            fields[0].to_string(),
            fields.get(3).copied().unwrap_or_default().to_string(),
        ));
    }
    candidates.sort_by(|left, right| left.0.total_cmp(&right.0));
    if candidates.is_empty() {
        return Err("timezone coordinate table contains no usable entries".to_string());
    }
    let suggestions = candidates
        .into_iter()
        .take(5)
        .map(|(distance_km, zone, countries, comment)| {
            json!({
                "zone": zone,
                "representative_distance_km": (distance_km * 10.0).round() / 10.0,
                "countries": countries,
                "comment": comment,
            })
        })
        .collect::<Vec<_>>();
    Ok(json!({
        "recommended": suggestions[0]["zone"],
        "candidates": suggestions,
        "current": current_timezone(),
        "method": "nearest zone1970.tab representative point",
        "advisory": true,
        "table": table,
    }))
}

fn parse_iso6709(value: &str) -> Option<(f64, f64)> {
    if value.len() < 10 || !matches!(value.as_bytes().first(), Some(b'+' | b'-')) {
        return None;
    }
    let split = value[1..]
        .bytes()
        .position(|byte| matches!(byte, b'+' | b'-'))?
        + 1;
    let latitude = parse_coordinate(&value[..split], 2)?;
    let longitude = parse_coordinate(&value[split..], 3)?;
    if (-90.0..=90.0).contains(&latitude) && (-180.0..=180.0).contains(&longitude) {
        Some((latitude, longitude))
    } else {
        None
    }
}

fn parse_coordinate(value: &str, degree_digits: usize) -> Option<f64> {
    let sign = match value.as_bytes().first()? {
        b'+' => 1.0,
        b'-' => -1.0,
        _ => return None,
    };
    let digits = &value[1..];
    let valid_length = digits.len() == degree_digits + 2 || digits.len() == degree_digits + 4;
    if !digits.bytes().all(|byte| byte.is_ascii_digit()) || !valid_length {
        return None;
    }
    let degrees = digits[..degree_digits].parse::<f64>().ok()?;
    let minutes = digits[degree_digits..degree_digits + 2]
        .parse::<f64>()
        .ok()?;
    let seconds = if digits.len() == degree_digits + 4 {
        digits[degree_digits + 2..].parse::<f64>().ok()?
    } else {
        0.0
    };
    if minutes >= 60.0 || seconds >= 60.0 {
        return None;
    }
    Some(sign * (degrees + minutes / 60.0 + seconds / 3600.0))
}

fn haversine_km(latitude: f64, longitude: f64, other_lat: f64, other_lon: f64) -> f64 {
    const EARTH_RADIUS_KM: f64 = 6371.0088;
    let latitude = latitude.to_radians();
    let other_lat = other_lat.to_radians();
    let delta_lat = other_lat - latitude;
    let delta_lon = (other_lon - longitude).to_radians();
    let a = (delta_lat / 2.0).sin().powi(2)
        + latitude.cos() * other_lat.cos() * (delta_lon / 2.0).sin().powi(2);
    2.0 * EARTH_RADIUS_KM * a.clamp(0.0, 1.0).sqrt().asin()
}

fn current_timezone() -> Option<String> {
    if let Ok(target) = fs::canonicalize("/etc/localtime") {
        if let Ok(relative) = target.strip_prefix("/usr/share/zoneinfo") {
            let value = relative
                .to_string_lossy()
                .trim_start_matches('/')
                .to_string();
            if !value.is_empty() {
                return Some(value);
            }
        }
    }
    fs::read_to_string("/etc/timezone")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn validate_helper_parent(fd: libc::c_int) -> Result<(), String> {
    if fd < 3 {
        return Err("invalid GeoClue broker socket fd".to_string());
    }
    let mut credentials: libc::ucred = unsafe { std::mem::zeroed() };
    let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    let rc = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            &mut credentials as *mut libc::ucred as *mut libc::c_void,
            &mut length,
        )
    };
    let peer_error = (rc != 0).then(std::io::Error::last_os_error);
    let close_result = unsafe { libc::close(fd) };
    if let Some(error) = peer_error {
        return Err(format!("verify GeoClue broker socket: {error}"));
    }
    if length as usize != std::mem::size_of::<libc::ucred>() {
        return Err(format!(
            "verify GeoClue broker socket: unexpected credential size {length}"
        ));
    }
    if close_result != 0 {
        return Err(format!(
            "close GeoClue broker socket: {}",
            std::io::Error::last_os_error()
        ));
    }
    let parent = unsafe { libc::getppid() };
    if parent <= 1 || credentials.uid != 0 || credentials.pid != parent {
        return Err("GeoClue helper was not launched by root clawd".to_string());
    }
    Ok(())
}

#[derive(Clone)]
struct UserEnvironment {
    uid: u32,
    gid: u32,
    home: PathBuf,
    username: String,
}

impl UserEnvironment {
    fn new(uid: u32, gid: u32, home: PathBuf) -> Result<Self, String> {
        let metadata = fs::symlink_metadata(&home)
            .map_err(|error| format!("inspect location user home {}: {error}", home.display()))?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() || metadata.uid() != uid {
            return Err(format!(
                "location user home {} is not a user-owned directory",
                home.display()
            ));
        }
        Ok(Self {
            uid,
            gid,
            home,
            username: username_for_uid(uid)?,
        })
    }
}

async fn run_user_command(
    program: PathBuf,
    args: Vec<String>,
    environment: UserEnvironment,
    timeout: Duration,
) -> Result<CommandOutput, String> {
    tokio::task::spawn_blocking(move || run_user_command_sync(program, args, environment, timeout))
        .await
        .map_err(|error| format!("location helper worker failed: {error}"))?
}

fn run_user_command_sync(
    program: PathBuf,
    mut args: Vec<String>,
    environment: UserEnvironment,
    timeout: Duration,
) -> Result<CommandOutput, String> {
    let (broker_socket, helper_socket) =
        UnixStream::pair().map_err(|error| format!("create GeoClue broker socket: {error}"))?;
    let helper_fd = helper_socket.as_raw_fd();
    args.push(helper_fd.to_string());
    let mut command = Command::new(&program);
    command
        .args(args)
        .env_clear()
        .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
        .env("HOME", &environment.home)
        .env("USER", &environment.username)
        .env("LOGNAME", &environment.username)
        .env("LC_ALL", "C.UTF-8")
        .current_dir("/")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let uid = environment.uid;
    let gid = environment.gid;
    let expected_parent = unsafe { libc::getpid() };
    unsafe {
        command.pre_exec(move || {
            if libc::fcntl(helper_fd, libc::F_SETFD, 0) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::setgroups(0, std::ptr::null()) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::setgid(gid) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::setuid(uid) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::getppid() != expected_parent {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "location broker exited before child setup completed",
                ));
            }
            if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            let limit = libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            if libc::setrlimit(libc::RLIMIT_CORE as _, &limit) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command
        .spawn()
        .map_err(|error| format!("launch {}: {error}", program.display()))?;
    drop(helper_socket);
    let _broker_socket = broker_socket;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "location helper stdout is unavailable".to_string())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "location helper stderr is unavailable".to_string())?;
    let stdout_reader = std::thread::spawn(move || read_bounded(stdout));
    let stderr_reader = std::thread::spawn(move || read_bounded(stderr));
    let deadline = Instant::now() + timeout;
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(25));
            }
            Ok(None) => {
                timed_out = true;
                let _ = child.kill();
                break child
                    .wait()
                    .map_err(|error| format!("wait for timed-out location helper: {error}"))?;
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("wait for location helper: {error}"));
            }
        }
    };
    let (stdout, stdout_truncated) = stdout_reader
        .join()
        .map_err(|_| "location helper stdout reader panicked".to_string())??;
    let (stderr, stderr_truncated) = stderr_reader
        .join()
        .map_err(|_| "location helper stderr reader panicked".to_string())??;
    if timed_out {
        return Err(format!(
            "GeoClue helper timed out after {}s",
            timeout.as_secs()
        ));
    }
    Ok(CommandOutput {
        status,
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
        stdout_truncated,
        stderr_truncated,
    })
}

fn read_bounded(mut reader: impl Read) -> Result<(Vec<u8>, bool), String> {
    let mut kept = Vec::new();
    let mut buffer = [0_u8; 16 * 1024];
    let mut truncated = false;
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|error| format!("read location helper output: {error}"))?;
        if read == 0 {
            break;
        }
        let remaining = STREAM_CAP_BYTES.saturating_sub(kept.len());
        let keep = remaining.min(read);
        kept.extend_from_slice(&buffer[..keep]);
        truncated |= keep < read;
    }
    Ok((kept, truncated))
}

struct CommandOutput {
    status: ExitStatus,
    stdout: String,
    stderr: String,
    #[allow(dead_code)]
    stdout_truncated: bool,
    #[allow(dead_code)]
    stderr_truncated: bool,
}

fn username_for_uid(uid: u32) -> Result<String, String> {
    use std::ffi::CStr;

    const BUF_SIZE: usize = 16 * 1024;
    let mut buffer = vec![0 as libc::c_char; BUF_SIZE];
    let mut passwd: libc::passwd = unsafe { std::mem::zeroed() };
    let mut result = std::ptr::null_mut();
    let rc = unsafe {
        libc::getpwuid_r(
            uid,
            &mut passwd,
            buffer.as_mut_ptr(),
            buffer.len(),
            &mut result,
        )
    };
    if rc != 0 || result.is_null() || passwd.pw_name.is_null() {
        return Err(format!("passwd entry is unavailable for uid {uid}"));
    }
    let username = unsafe { CStr::from_ptr(passwd.pw_name) }
        .to_str()
        .map_err(|_| format!("username is not UTF-8 for uid {uid}"))?
        .to_string();
    if username.is_empty() {
        return Err(format!("username is empty for uid {uid}"));
    }
    Ok(username)
}

fn optional_string(params: &Value, key: &str) -> Result<Option<String>, String> {
    match params.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if !value.is_empty() => Ok(Some(value.clone())),
        Some(Value::String(_)) => Ok(None),
        Some(_) => Err(format!("parameter `{key}` must be a string or null")),
    }
}

fn required_string(params: &Value, key: &str) -> Result<String, String> {
    optional_string(params, key)?.ok_or_else(|| format!("missing required string parameter: {key}"))
}

fn tail(value: &str) -> String {
    const MAX: usize = 8 * 1024;
    if value.len() <= MAX {
        return value.trim().to_string();
    }
    let mut start = value.len() - MAX;
    while !value.is_char_boundary(start) {
        start += 1;
    }
    value[start..].trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_zone_table_coordinates() {
        let (latitude, longitude) = parse_iso6709("+404251-0740023").unwrap();
        assert!((latitude - 40.714_166).abs() < 0.001);
        assert!((longitude + 74.006_388).abs() < 0.001);
        assert!(parse_iso6709("invalid").is_none());
    }

    #[test]
    fn maps_accuracy_names_exactly() {
        assert_eq!(accuracy_level("city").unwrap(), 4);
        assert_eq!(accuracy_level("exact").unwrap(), 8);
        assert!(accuracy_level("gps").is_err());
    }
}
