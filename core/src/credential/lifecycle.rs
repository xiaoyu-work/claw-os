use super::*;

pub(super) fn load_credential(
    store: &FileCredentialStore,
    id: &CredentialId,
) -> Result<LoadedCredential, String> {
    if !store.contains(id)? {
        return Err(format!("credential not found: {}", id.name()));
    }
    let credential = store
        .read_record(id)?
        .ok_or_else(|| format!("credential not found: {}", id.name()))?;
    let current_tier = effective_session_tier();
    require_credential_access(&credential, id.namespace(), id.name(), current_tier)?;

    if is_expired(&credential.expires_at) {
        let Some(refresh_cmd) = credential.refresh_cmd.as_ref() else {
            return Err(serde_json::to_string(&json!({
                "error": format!("credential '{}' has expired", id.name()),
                "expired": true,
                "expires_at": credential.expires_at,
            }))
            .unwrap_or_else(|_| format!("credential '{}' has expired", id.name())));
        };

        return store.with_refresh(id, || {
            let fresh = store
                .read_record(id)?
                .ok_or_else(|| format!("credential '{}' disappeared during refresh", id.name()))?;
            require_credential_access(&fresh, id.namespace(), id.name(), current_tier)?;
            if !is_expired(&fresh.expires_at) {
                return loaded(id, &fresh, Some(false));
            }

            let refresh_cmd = fresh
                .refresh_cmd
                .clone()
                .unwrap_or_else(|| refresh_cmd.clone());
            let command_output = match broker_oauth_provider(&refresh_cmd, id.namespace()) {
                Some(provider) => {
                    let direct_admin =
                        crate::proc::current_session_info_for_caps().is_some_and(|session| {
                            oauth_login::is_same_pid_admin_cli_session(&session)
                        });
                    if direct_admin {
                        direct_oauth_refresh(store, provider, id.namespace()).map_err(|error| {
                            format!(
                                "credential '{}' expired and auto-refresh failed: {error}",
                                id.name()
                            )
                        })?;
                    } else if crate::proc::current_session_id().is_some() {
                        request_brokered_oauth_refresh(id.name(), id.namespace()).map_err(
                            |error| {
                                format!(
                                    "credential '{}' expired and broker refresh failed: {error}",
                                    id.name()
                                )
                            },
                        )?;
                    } else {
                        direct_oauth_refresh(store, provider, id.namespace()).map_err(|error| {
                            format!(
                                "credential '{}' expired and auto-refresh failed: {error}",
                                id.name()
                            )
                        })?;
                    }
                    None
                }
                None => Some(execute_refresh(&refresh_cmd).map_err(|error| {
                    format!(
                        "credential '{}' expired and auto-refresh failed: {error}",
                        id.name()
                    )
                })?),
            };

            if let Some(after) = store.read_record(id)? {
                require_credential_access(&after, id.namespace(), id.name(), current_tier)?;
                if after.value_b64 != fresh.value_b64
                    || after.nonce_b64 != fresh.nonce_b64
                    || !is_expired(&after.expires_at)
                {
                    return loaded(id, &after, Some(true));
                }
            }

            let new_value = command_output.ok_or_else(|| {
                format!(
                    "credential '{}' OAuth broker completed without updating the access token",
                    id.name()
                )
            })?;
            let ttl = compute_original_ttl(&fresh);
            let (value_b64, nonce_b64) = encrypt_value(new_value.trim().as_bytes())?;
            let now = chrono::Utc::now();
            let expires_at = ttl.map(|seconds| {
                (now + chrono::Duration::seconds(seconds))
                    .format("%Y-%m-%dT%H:%M:%SZ")
                    .to_string()
            });
            let updated = StoredCredential {
                name: fresh.name.clone(),
                namespace: fresh.namespace.clone(),
                value_b64,
                nonce_b64: Some(nonce_b64),
                min_tier: fresh.min_tier,
                stored_at: fresh.stored_at.clone(),
                stored_by: fresh.stored_by.clone(),
                expires_at,
                refresh_cmd: fresh.refresh_cmd.clone(),
            };
            store
                .write_record(&updated)
                .map_err(|error| format!("failed to write refreshed credential: {error}"))?;
            loaded(id, &updated, Some(true)).map(|mut result| {
                result.value = new_value.trim().to_string();
                result
            })
        });
    }

    loaded(id, &credential, None)
}

fn loaded(
    id: &CredentialId,
    credential: &StoredCredential,
    refreshed: Option<bool>,
) -> Result<LoadedCredential, String> {
    let value = String::from_utf8(decrypt_value(credential)?)
        .map_err(|error| format!("credential is not valid UTF-8: {error}"))?;
    Ok(LoadedCredential {
        name: id.name().to_string(),
        namespace: credential.namespace.clone(),
        min_tier: credential.min_tier,
        value,
        refreshed,
        expires_at: credential.expires_at.clone(),
    })
}

pub(super) fn load_bundle(
    store: &FileCredentialStore,
    manifest: &BundleManifest,
    current_tier: u8,
) -> LoadedBundle {
    let mut credentials = std::collections::BTreeMap::new();
    let mut errors = std::collections::BTreeMap::new();
    for name in &manifest.keys {
        let id = match CredentialId::parse(&manifest.namespace, name) {
            Ok(id) => id,
            Err(error) => {
                errors.insert(name.clone(), error);
                continue;
            }
        };
        let credential = match store.read_record(&id) {
            Ok(Some(credential)) => credential,
            Ok(None) => {
                errors.insert(name.clone(), format!("credential not found: {name}"));
                continue;
            }
            Err(error) => {
                errors.insert(name.clone(), format!("failed to read or parse: {error}"));
                continue;
            }
        };
        if credential.name != *name || credential.namespace != manifest.namespace {
            errors.insert(
                name.clone(),
                "credential metadata does not match its storage path".to_string(),
            );
            continue;
        }
        if !tier_grants_access(current_tier, credential.min_tier) {
            errors.insert(
                name.clone(),
                format!(
                    "insufficient tier: requires {}, have {}",
                    credential.min_tier, current_tier
                ),
            );
            continue;
        }
        if is_expired(&credential.expires_at) {
            errors.insert(name.clone(), "credential has expired".to_string());
            continue;
        }
        match decrypt_value(&credential).and_then(|bytes| {
            String::from_utf8(bytes).map_err(|error| format!("not valid UTF-8: {error}"))
        }) {
            Ok(value) => {
                credentials.insert(name.clone(), value);
            }
            Err(error) => {
                errors.insert(name.clone(), error);
            }
        }
    }
    LoadedBundle {
        credentials,
        errors,
    }
}

// ===========================================================================
// Auto-refresh helpers
// ===========================================================================

/// Execute a refresh command and capture its stdout as the new value.
pub(super) fn execute_refresh(cmd: &str) -> Result<String, String> {
    use std::process::{Command, Stdio};

    // OS safety: only allow cos commands as refresh commands.
    // This prevents arbitrary code execution from credential files.
    let trimmed = cmd.trim();
    if !trimmed.starts_with("cos ") && !trimmed.starts_with("cos\t") && trimmed != "cos" {
        return Err(format!(
            "refresh_cmd must be a cos command (starts with 'cos '). got: {}",
            &trimmed[..trimmed.len().min(50)]
        ));
    }

    // Execute via direct argv, not shell — no injection possible
    let parts: Vec<&str> = trimmed.split_whitespace().collect();
    let output = Command::new(parts[0])
        .args(&parts[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("failed to execute refresh command: {e}"))?;

    if output.status.success() {
        let value = String::from_utf8(output.stdout)
            .map_err(|e| format!("refresh output not valid UTF-8: {e}"))?;
        if value.trim().is_empty() {
            return Err("refresh command produced empty output".into());
        }
        Ok(value)
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(format!(
            "refresh command failed (exit {}): {}",
            output.status.code().unwrap_or(-1),
            stderr.trim()
        ))
    }
}

/// Compute the original TTL from stored_at and expires_at.
pub(super) fn compute_original_ttl(cred: &StoredCredential) -> Option<i64> {
    let expires_str = cred.expires_at.as_ref()?;
    let stored =
        chrono::DateTime::parse_from_rfc3339(&cred.stored_at.replace('Z', "+00:00")).ok()?;
    let expires = chrono::DateTime::parse_from_rfc3339(&expires_str.replace('Z', "+00:00")).ok()?;
    let duration = expires.signed_duration_since(stored);
    Some(duration.num_seconds())
}
