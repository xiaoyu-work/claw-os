use super::*;

// ===========================================================================
// Argument parsing helpers
// ===========================================================================

/// Extract `--namespace <value>` from an argument list.
/// Returns `(namespace_option, remaining_args)`.
pub(super) fn parse_namespace_flag(args: &[String]) -> (Option<String>, Vec<String>) {
    let mut ns: Option<String> = None;
    let mut rest = Vec::new();
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--namespace" && i + 1 < args.len() {
            ns = Some(args[i + 1].clone());
            i += 2;
        } else {
            rest.push(args[i].clone());
            i += 1;
        }
    }
    (ns, rest)
}

// ===========================================================================
// Command dispatch
// ===========================================================================

pub fn run(command: &str, args: &[String]) -> Result<Value, String> {
    match command {
        "store" => cmd_store(args),
        "load" => cmd_load(args),
        "revoke" => cmd_revoke(args),
        "list" => cmd_list(args),
        "bundle" => cmd_bundle(args),
        "load-bundle" => cmd_load_bundle(args),
        "oauth-login" => oauth_login::cmd_oauth_login(&FILE_STORE, args),
        "oauth-refresh" => cmd_oauth_refresh(&FILE_STORE, args),
        _ => Err(format!("unknown credential command: {command}")),
    }
}

pub(crate) fn run_agent_oauth_login(args: &[String]) -> Result<Value, String> {
    oauth_login::cmd_agent_oauth_login(&FILE_STORE, args)
}

// ===========================================================================
// Commands
// ===========================================================================

/// Store a credential.
///
/// Usage: cos credential store <name> <value> [--tier N] [--namespace NS] [--ttl SECS]
pub(super) fn cmd_store(args: &[String]) -> Result<Value, String> {
    let (ns_opt, args) = parse_namespace_flag(args);
    let namespace = ns_opt.unwrap_or_else(|| "default".into());

    let mut min_tier = effective_session_tier();
    if min_tier > 3 {
        return Err("active session has no valid credential tier".to_string());
    }
    let mut ttl: Option<u64> = None;
    let mut refresh_cmd: Option<String> = None;
    let mut positional: Vec<String> = Vec::new();

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--tier" if i + 1 < args.len() => {
                min_tier = args[i + 1]
                    .parse::<u8>()
                    .map_err(|_| "tier must be 0-3".to_string())?;
                if min_tier > 3 {
                    return Err("tier must be 0-3".into());
                }
                i += 2;
            }
            "--ttl" if i + 1 < args.len() => {
                ttl = Some(
                    args[i + 1]
                        .parse::<u64>()
                        .map_err(|_| "ttl must be a positive integer (seconds)".to_string())?,
                );
                i += 2;
            }
            "--refresh-cmd" if i + 1 < args.len() => {
                let cmd = args[i + 1].trim().to_string();
                if !cmd.starts_with("cos ") {
                    return Err("--refresh-cmd must be a cos command (e.g., 'cos credential oauth-refresh google')".into());
                }
                refresh_cmd = Some(cmd);
                i += 2;
            }
            _ => {
                positional.push(args[i].clone());
                i += 1;
            }
        }
    }

    if positional.len() < 2 {
        return Err(
            "usage: cos credential store <name> <value> [--tier N] [--namespace NS] [--ttl SECS] [--refresh-cmd CMD]"
                .into(),
        );
    }

    let name = &positional[0];
    let value = &positional[1];

    let id = CredentialId::parse(&namespace, name)?;
    let scope = credential_scope(id.namespace(), id.name())?;
    require_secret(Verb::SECRET_WRITE, scope)?;

    let stored = FILE_STORE.store(StoreRequest {
        id: &id,
        value,
        min_tier,
        ttl,
        refresh_cmd,
    })?;
    let mut result = json!({
        "stored": id.name(),
        "namespace": id.namespace(),
        "min_tier": min_tier,
        "stored_at": stored.stored_at,
    });
    if let Some(expires_at) = stored.expires_at {
        result["expires_at"] = json!(expires_at);
    }
    Ok(result)
}

pub(super) fn cmd_revoke(args: &[String]) -> Result<Value, String> {
    let (namespace, rest) = parse_namespace_flag(args);
    let namespace = namespace.unwrap_or_else(|| "default".into());
    let name = rest.first().ok_or("usage: cos credential revoke <name>")?;
    let id = CredentialId::parse(&namespace, name)?;
    require_secret(
        Verb::SECRET_WRITE,
        credential_scope(id.namespace(), id.name())?,
    )?;

    if !revoke(&id).map_err(|error| format!("failed to revoke credential: {error}"))? {
        return Err(format!("credential not found: {name}"));
    }

    Ok(json!({
        "revoked": id.name(),
        "namespace": id.namespace(),
    }))
}

pub(super) fn cmd_list(args: &[String]) -> Result<Value, String> {
    let (namespace, _rest) = parse_namespace_flag(args);
    match namespace {
        Some(namespace) => {
            require_secret(Verb::SECRET_READ, namespace_scope(&namespace)?)?;
            let namespace = NamespaceId::parse(&namespace)?;
            let credentials = list_namespace(&namespace)?
                .into_iter()
                .map(|credential| {
                    let mut value = json!({
                        "name": credential.name,
                        "min_tier": credential.min_tier,
                        "stored_at": credential.stored_at,
                        "stored_by": credential.stored_by,
                        "expired": credential.expired,
                    });
                    if let Some(expires_at) = credential.expires_at {
                        value["expires_at"] = json!(expires_at);
                    }
                    if let Some(refresh_cmd) = credential.refresh_cmd {
                        value["refresh_cmd"] = json!(refresh_cmd);
                    }
                    value
                })
                .collect::<Vec<_>>();
            Ok(json!({
                "namespace": namespace.as_str(),
                "count": credentials.len(),
                "credentials": credentials,
            }))
        }
        None => {
            require_secret(Verb::SECRET_READ, Scope::name("**"))?;
            let namespaces = list_all_namespaces()?;
            let total = namespaces.iter().map(|entry| entry.count).sum::<usize>();
            Ok(json!({
                "namespaces": namespaces
                    .into_iter()
                    .map(|entry| json!({
                        "namespace": entry.namespace,
                        "count": entry.count,
                    }))
                    .collect::<Vec<_>>(),
                "total": total,
            }))
        }
    }
}

/// Load a credential value.
///
/// Usage: cos credential load <name> [--namespace NS] [--fd N]
///
/// `--fd N` writes the raw plaintext bytes to file descriptor `N` (no
/// trailing newline) and omits the `"value"` field from the returned JSON,
/// so callers can capture secrets without ever piping them through
/// stdout / shell history / IPC log sinks. See the audit's MEDIUM "secret
/// values returned in JSON cross IPC boundary" finding.
pub(super) fn cmd_load(args: &[String]) -> Result<Value, String> {
    let (namespace, rest) = parse_namespace_flag(args);
    let namespace = namespace.unwrap_or_else(|| "default".into());
    let mut fd_target = None;
    let mut positional = Vec::new();
    let mut index = 0;
    while index < rest.len() {
        match rest[index].as_str() {
            "--fd" if index + 1 < rest.len() => {
                let fd = rest[index + 1]
                    .parse::<i32>()
                    .map_err(|_| "--fd must be a non-negative integer".to_string())?;
                if fd < 0 {
                    return Err("--fd must be a non-negative integer".into());
                }
                fd_target = Some(fd);
                index += 2;
            }
            _ => {
                positional.push(rest[index].clone());
                index += 1;
            }
        }
    }

    let name = positional
        .first()
        .ok_or("usage: cos credential load <name>")?;
    let id = CredentialId::parse(&namespace, name)?;
    require_secret(
        Verb::SECRET_READ,
        credential_scope(id.namespace(), id.name())?,
    )?;
    build_load_result(load_credential(&FILE_STORE, &id)?, fd_target)
}

/// Build the JSON response for `cmd_load` (or its auto-refresh path).
///
/// If `fd_target` is `Some(n)`, the plaintext is written raw (no trailing
/// newline) to file descriptor `n` and the `"value"` key is replaced by
/// `"value_fd": n` so the secret never crosses the IPC / stdout boundary.
/// Otherwise the value is embedded in the JSON as before.
fn build_load_result(loaded: LoadedCredential, fd_target: Option<i32>) -> Result<Value, String> {
    let mut result = json!({
        "name": loaded.name,
        "namespace": loaded.namespace,
        "min_tier": loaded.min_tier,
    });

    if let Some(refreshed_flag) = loaded.refreshed {
        result["refreshed"] = json!(refreshed_flag);
        if let Some(ref exp) = loaded.expires_at {
            result["expires_at"] = json!(exp);
        }
    }

    match fd_target {
        Some(fd) => {
            write_value_to_fd(fd, loaded.value.as_bytes())?;
            result["value_fd"] = json!(fd);
        }
        None => {
            result["value"] = json!(loaded.value);
        }
    }
    Ok(result)
}

/// Write `bytes` raw (no newline) to file descriptor `fd`. Used by the
/// `--fd N` mode of `cmd_load` so secrets can be handed off to a caller
/// via an out-of-band fd that the caller has set up specifically for the
/// transfer — never via stdout (where shell history / pipes / audit sinks
/// can capture them).
#[cfg(unix)]
fn write_value_to_fd(fd: i32, bytes: &[u8]) -> Result<(), String> {
    let mut remaining = bytes;
    while !remaining.is_empty() {
        let n = unsafe {
            libc::write(
                fd,
                remaining.as_ptr() as *const libc::c_void,
                remaining.len(),
            )
        };
        if n < 0 {
            return Err(format!(
                "failed to write to fd {fd}: {}",
                std::io::Error::last_os_error()
            ));
        }
        if n == 0 {
            return Err(format!("write to fd {fd} returned 0 bytes"));
        }
        remaining = &remaining[(n as usize)..];
    }
    Ok(())
}

#[cfg(not(unix))]
fn write_value_to_fd(_fd: i32, _bytes: &[u8]) -> Result<(), String> {
    Err("--fd is only supported on Unix".into())
}

/// Create a credential bundle — a named group of credential keys.
///
/// Usage: cos credential bundle <bundle-name> --keys key1,key2,key3 [--namespace NS]
pub(super) fn cmd_bundle(args: &[String]) -> Result<Value, String> {
    let (ns_opt, rest) = parse_namespace_flag(args);
    let namespace = ns_opt.unwrap_or_else(|| "default".into());

    let mut keys: Option<String> = None;
    let mut positional: Vec<String> = Vec::new();

    let mut i = 0;
    while i < rest.len() {
        match rest[i].as_str() {
            "--keys" if i + 1 < rest.len() => {
                keys = Some(rest[i + 1].clone());
                i += 2;
            }
            _ => {
                positional.push(rest[i].clone());
                i += 1;
            }
        }
    }

    let bundle_name = positional
        .first()
        .ok_or("usage: cos credential bundle <name> --keys key1,key2,key3 [--namespace NS]")?;

    let keys_str = keys.ok_or("--keys is required (comma-separated list of credential names)")?;
    let key_list: Vec<String> = keys_str.split(',').map(|s| s.trim().to_string()).collect();

    if key_list.is_empty() {
        return Err("--keys must specify at least one credential name".into());
    }
    validate_credential_component("bundle name", bundle_name)?;
    for key in &key_list {
        validate_credential_component("credential name", key)?;
    }
    require_secret(Verb::SECRET_GRANT, bundle_scope(&namespace, bundle_name)?)?;
    for key in &key_list {
        require_secret(Verb::SECRET_GRANT, credential_scope(&namespace, key)?)?;
    }

    let namespace_id = NamespaceId::parse(&namespace)?;
    let manifest = FILE_STORE.write_bundle(&namespace_id, bundle_name, key_list)?;

    Ok(json!({
        "bundle": manifest.name,
        "namespace": manifest.namespace,
        "keys": manifest.keys,
        "created_at": manifest.created_at,
    }))
}

/// Load all credentials in a bundle as a JSON object.
///
/// Usage: cos credential load-bundle <bundle-name> [--namespace NS]
pub(super) fn cmd_load_bundle(args: &[String]) -> Result<Value, String> {
    let (ns_opt, rest) = parse_namespace_flag(args);
    let namespace = ns_opt.unwrap_or_else(|| "default".into());

    let bundle_name = rest
        .first()
        .ok_or("usage: cos credential load-bundle <name> [--namespace NS]")?;
    let namespace_id = NamespaceId::parse(&namespace)?;
    validate_credential_component("bundle name", bundle_name)?;
    require_secret(Verb::SECRET_READ, bundle_scope(&namespace, bundle_name)?)?;

    let manifest = FILE_STORE
        .read_bundle(&namespace_id, bundle_name)?
        .ok_or_else(|| format!("bundle not found: {bundle_name}"))?;
    if manifest.name != *bundle_name || manifest.namespace != namespace {
        return Err("bundle metadata does not match its storage path".to_string());
    }

    // A bundle is grouping metadata, not an authority container. Authorize
    // every member before reading any file so bundle scope can never widen a
    // session's per-secret grants or produce a partial authorization oracle.
    for key in &manifest.keys {
        validate_credential_component("credential name", key)?;
        require_secret(Verb::SECRET_READ, credential_scope(&namespace, key)?)?;
    }

    let loaded = load_bundle(&FILE_STORE, &manifest, effective_session_tier());

    let mut result = json!({
        "bundle": bundle_name,
        "namespace": namespace,
        "credentials": loaded.credentials,
    });
    if !loaded.errors.is_empty() {
        result["errors"] = json!(loaded.errors);
    }
    Ok(result)
}
