use serde_json::{json, Value};
use std::io;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt};

use super::super::authority::{Authorized, Decision};
use super::super::client_identity::ClientIdentity;
use super::super::protocol::BrokerError;
use super::super::wire::requests::PackageQuery;
use crate::caps::{Cap, Scope, Verb};

const TIMEOUT: Duration = Duration::from_secs(20);
const MAX_OUTPUT: usize = 8 * 1024 * 1024;
const MAX_ERROR: usize = 64 * 1024;
const MAX_WORKSPACE: u64 = 256 * 1024 * 1024;

pub(crate) fn cli(args: &[String]) -> Result<Value, String> {
    let session = std::env::var("COS_SESSION")
        .map_err(|_| "package queries require an authenticated App session")?;
    let body = match args {
        [action] if action == "installed" => json!({"session": session, "action": action}),
        [action, name] if matches!(action.as_str(), "show" | "available") => {
            json!({"session": session, "action": action, "name": name})
        }
        [action, query, limit] if action == "search" => {
            let limit = limit
                .parse::<u16>()
                .map_err(|_| "invalid package search limit")?;
            json!({"session": session, "action": action, "query": query, "limit": limit})
        }
        _ => {
            return Err(
                "usage: internal package query <installed | show name | available name | search query limit>".into(),
            );
        }
    };
    let request: PackageQuery =
        serde_json::from_value(body.clone()).map_err(|_| "invalid package query")?;
    required_cap(&request)?;
    Ok(body)
}

pub(super) fn required_cap(request: &PackageQuery) -> Result<Cap, String> {
    let scope = match (
        request.action.as_str(),
        request.name.as_ref(),
        request.query.as_ref(),
        request.limit,
    ) {
        ("installed", None, None, None) => Scope::name("packages"),
        ("show", Some(name), None, None) => {
            super::validate_package_name(name.as_str())?;
            Scope::name(name.as_str())
        }
        ("available", Some(name), None, None)
            if !name.as_str().trim().is_empty()
                && !name.as_str().starts_with('-')
                && !name.as_str().contains('/')
                && !matches!(name.as_str(), "." | "..")
                && !name.as_str().chars().any(char::is_control) =>
        {
            Scope::name(name.as_str())
        }
        ("search", None, Some(query), Some(1..=100))
            if !query.as_str().trim().is_empty()
                && !query.as_str().chars().any(char::is_control) =>
        {
            Scope::name("packages")
        }
        _ => return Err("invalid closed package query or search limit (1..100)".into()),
    };
    Ok(Cap::new(Verb::SYS_OBSERVE, scope))
}

pub(super) fn authorize(
    authority: &Decision,
    owner: u32,
    request: &PackageQuery,
) -> Result<Authorized, String> {
    if authority.owner_uid() != owner {
        return Err("package observation belongs to another owner".into());
    }
    authority.require(required_cap(request)?)
}

pub(crate) async fn query(
    params: Value,
    client: &ClientIdentity,
    authority: &Decision,
) -> Result<Value, BrokerError> {
    let request: PackageQuery = serde_json::from_value(params)
        .map_err(|_| BrokerError::execution("Invalid package query"))?;
    let owner = client.require_uid().map_err(BrokerError::authorization)?;
    let _authorized = authorize(authority, owner, &request).map_err(BrokerError::authorization)?;
    if request.action.as_str() == "available" {
        return availability(request.name.as_ref().unwrap().as_str(), owner).await;
    }
    let (path, args) = match request.action.as_str() {
        "search" => (
            "/usr/bin/apt-cache",
            vec![
                "-o",
                "Dir::Cache::pkgcache=",
                "-o",
                "Dir::Cache::srcpkgcache=",
                "search",
                "--names-only",
                "--",
                request.query.as_ref().unwrap().as_str(),
            ],
        ),
        "show" => (
            "/usr/bin/apt-cache",
            vec![
                "-o",
                "Dir::Cache::pkgcache=",
                "-o",
                "Dir::Cache::srcpkgcache=",
                "show",
                "--",
                request.name.as_ref().unwrap().as_str(),
            ],
        ),
        "installed" => (
            "/usr/bin/dpkg-query",
            vec![
                "--show",
                "--showformat=${binary:Package}\t${db:Status-Status}\t${Version}\n",
            ],
        ),
        _ => unreachable!("authorized closed package query"),
    };
    let output = execute(path, &args, owner)
        .await
        .map_err(BrokerError::unavailable)?;
    if !output.status.success() {
        if request.action.as_str() == "show"
            && output.status.code() == Some(100)
            && output.stdout.is_empty()
            && output
                .stderr
                .lines()
                .any(|line| line == "E: No packages found")
        {
            return Ok(json!({
                "name": request.name.as_ref().unwrap().as_str(),
                "found": false, "error": "no package found",
            }));
        }
        return Err(BrokerError::unavailable(format!(
            "The system package metadata query failed ({})",
            output.status,
        )));
    }
    match request.action.as_str() {
        "search" => search_result(
            request.query.as_ref().unwrap().as_str(),
            usize::from(request.limit.unwrap()),
            &output.stdout,
        ),
        "show" => show_result(request.name.as_ref().unwrap().as_str(), &output.stdout),
        "installed" => installed_result(&output.stdout),
        _ => unreachable!("authorized closed package query"),
    }
    .map_err(BrokerError::unavailable)
}

async fn availability(name: &str, owner: u32) -> Result<Value, BrokerError> {
    if super::validate_package_name(name).is_ok() {
        let output = execute(
            "/usr/bin/dpkg-query",
            &["--show", "--showformat=${db:Status-Status}\n", "--", name],
            owner,
        )
        .await
        .map_err(BrokerError::unavailable)?;
        if output.status.success() && output.stdout.lines().any(|line| line == "installed") {
            return Ok(json!({
                "name": name, "available": true, "type": "system",
                "details": "installed via dpkg",
            }));
        }
        if !output.status.success()
            && !(output.status.code() == Some(1) && output.stdout.is_empty())
        {
            return Err(BrokerError::unavailable(
                "The package availability query failed",
            ));
        }
    }
    let output = execute(
        "/usr/bin/python3",
        &[
            "-I", "-S", "-B", "-c",
            "import json, shutil, sys; print(json.dumps(shutil.which(sys.argv[1], path='/usr/local/bin:/usr/bin:/bin')))",
            name,
        ],
        owner,
    )
    .await
    .map_err(BrokerError::unavailable)?;
    if !output.status.success() {
        return Err(BrokerError::unavailable(
            "The command availability query failed",
        ));
    }
    let path: Option<String> = serde_json::from_str(&output.stdout)
        .map_err(|_| BrokerError::unavailable("Invalid command availability metadata"))?;
    Ok(match path {
        Some(path) => json!({
            "name": name, "available": true, "type": "command",
            "details": format!("found at {path}"),
        }),
        None => json!({"name": name, "available": false, "type": "", "details": "not found"}),
    })
}

struct Output {
    status: std::process::ExitStatus,
    stdout: String,
    stderr: String,
}

async fn read_output(reader: impl AsyncRead + Unpin, limit: usize) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader
        .take((limit + 1) as u64)
        .read_to_end(&mut bytes)
        .await?;
    Ok(bytes)
}

#[cfg(target_os = "linux")]
async fn execute(path: &str, args: &[&str], owner: u32) -> Result<Output, String> {
    let group = super::super::client_identity::owner_groups(owner)
        .map_err(|_| "The package query owner identity is unavailable")?
        .0;
    let parent = std::process::id() as libc::pid_t;
    let mut command = tokio::process::Command::new(path);
    command
        .args(args)
        .env_clear()
        .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
        .env("HOME", "/")
        .env("LC_ALL", "C")
        .current_dir("/")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    unsafe {
        command.pre_exec(move || {
            crate::agentd::spawn::mark_inherited_descriptors_cloexec_except(3, -1);
            if libc::setgroups(0, std::ptr::null()) != 0
                || libc::setgid(group) != 0
                || libc::setuid(owner) != 0
                || libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0
                || libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0) != 0
                || libc::getppid() != parent
            {
                return Err(io::Error::other("Package query identity isolation failed"));
            }
            for (resource, maximum) in [
                (libc::RLIMIT_AS, MAX_WORKSPACE),
                (libc::RLIMIT_CPU, 10),
                (libc::RLIMIT_CORE, 0),
                // APT reads can need temporary backing even with persistent caches disabled.
                (libc::RLIMIT_FSIZE, MAX_WORKSPACE),
            ] {
                let limit = libc::rlimit {
                    rlim_cur: maximum,
                    rlim_max: maximum,
                };
                if libc::setrlimit(resource, &limit) != 0 {
                    return Err(io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
    let mut child = command
        .spawn()
        .map_err(|_| "The system package metadata backend is unavailable")?;
    let stdout = child.stdout.take().expect("piped package stdout");
    let stderr = child.stderr.take().expect("piped package stderr");
    let result = tokio::time::timeout(TIMEOUT, async {
        tokio::try_join!(
            child.wait(),
            read_output(stdout, MAX_OUTPUT),
            read_output(stderr, MAX_ERROR),
        )
    })
    .await;
    let (status, stdout, stderr) = match result {
        Ok(Ok(output)) => output,
        failure => {
            child
                .kill()
                .await
                .map_err(|_| "Package query cleanup is unconfirmed")?;
            return Err(if failure.is_err() {
                "The system package metadata query timed out"
            } else {
                "The system package metadata query could not be read"
            }
            .into());
        }
    };
    if stdout.len() > MAX_OUTPUT || stderr.len() > MAX_ERROR {
        return Err("The system package metadata exceeds its output bound".into());
    }
    Ok(Output {
        status,
        stdout: String::from_utf8(stdout).map_err(|_| "Package metadata is not UTF-8")?,
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
    })
}

#[cfg(not(target_os = "linux"))]
async fn execute(_path: &str, _args: &[&str], _owner: u32) -> Result<Output, String> {
    Err("System package metadata requires Linux".into())
}

pub(super) fn search_result(query: &str, limit: usize, text: &str) -> Result<Value, String> {
    let mut results = Vec::new();
    let mut truncated = false;
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        if results.len() == limit {
            truncated = true;
            break;
        }
        let (name, summary) = line.split_once(" - ").unwrap_or((line, ""));
        super::validate_package_name(name)?;
        results.push(json!({"name": name, "summary": summary}));
    }
    let mut value = json!({"query": query, "count": results.len(), "results": results});
    if truncated {
        value["truncated"] = json!(true);
        value["hint"] = json!(format!(
            "showing first {limit} results; pass --limit N (max 100) to widen"
        ));
    }
    Ok(value)
}

pub(super) fn show_result(name: &str, text: &str) -> Result<Value, String> {
    let mut fields = std::collections::BTreeMap::<String, String>::new();
    let mut key = String::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            if !fields.is_empty() {
                break;
            }
        } else if line.starts_with([' ', '\t']) {
            if let Some(value) = fields.get_mut(&key) {
                value.push('\n');
                value.push_str(line.trim());
            }
        } else if let Some((field, value)) = line.split_once(':') {
            key = field.to_string();
            fields.insert(key.clone(), value.trim().to_string());
        }
    }
    let Some(package) = fields.get("Package") else {
        return Err("The system package metadata has no package identity".into());
    };
    super::validate_package_name(package)?;
    if package != name.split(':').next().unwrap_or(name) {
        return Err("The system package metadata returned another package".into());
    }
    let description = fields.get("Description").map(String::as_str).unwrap_or("");
    let (summary, description) = description.split_once('\n').unwrap_or((description, ""));
    Ok(json!({
        "name": package, "found": true,
        "version": fields.get("Version").map(String::as_str).unwrap_or(""),
        "summary": summary, "description": description,
        "section": fields.get("Section").map(String::as_str).unwrap_or(""),
        "homepage": fields.get("Homepage").map(String::as_str).unwrap_or(""),
        "depends": fields.get("Depends").map(String::as_str).unwrap_or(""),
        "installed_size": fields.get("Installed-Size").map(String::as_str).unwrap_or(""),
        "maintainer": fields.get("Maintainer").map(String::as_str).unwrap_or(""),
    }))
}

pub(super) fn installed_result(text: &str) -> Result<Value, String> {
    let mut packages = Vec::new();
    let mut versions = std::collections::BTreeMap::new();
    for line in text.lines() {
        let fields = line.split('\t').collect::<Vec<_>>();
        if fields.len() != 3 {
            return Err("The installed package metadata has an invalid shape".into());
        }
        super::validate_package_name(fields[0])?;
        if fields[1] == "installed" {
            packages.push(fields[0]);
            versions.insert(fields[0], fields[2]);
        }
    }
    Ok(json!({"packages": packages, "versions": versions}))
}
