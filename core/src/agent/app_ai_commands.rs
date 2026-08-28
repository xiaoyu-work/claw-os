use crate::apps;
use serde_json::{json, Value};

/// `cos agent budget` — inspect per-app AI spend.
///
/// Subcommands:
///   show <app>          Current period: used vs cap.
///   reset <app>         Roll over to next period (clears used).
///   history <app>       List past periods.
///
/// The system agent's usage is rolled up under the pseudo-app id
/// `system.agent`.
pub(super) fn budget_cmd(args: &[String]) -> Result<Value, String> {
    use crate::ai::{budget, user_budget};

    let sub = args.first().map(String::as_str).unwrap_or("");
    match sub {
        "show" => {
            let app = args
                .get(1)
                .ok_or_else(|| "usage: cos agent budget show <app>".to_string())?;
            let store = budget::Store::open()?;
            let snap = store.current(app).map_err(|e| e.to_string())?;
            Ok(json!({
                "app": app,
                "period": snap.period,
                "units_used": snap.units_used,
            }))
        }
        "reset" => {
            let app = args
                .get(1)
                .ok_or_else(|| "usage: cos agent budget reset <app>".to_string())?;
            let store = budget::Store::open()?;
            store.reset(app).map_err(|e| e.to_string())?;
            Ok(json!({"app": app, "reset": true}))
        }
        "history" => {
            let app = args
                .get(1)
                .ok_or_else(|| "usage: cos agent budget history <app>".to_string())?;
            let store = budget::Store::open()?;
            let rows = store.history(app).map_err(|e| e.to_string())?;
            Ok(json!({"app": app, "history": rows}))
        }
        "user" => {
            // `cos agent budget user <show|path>` — inspect the per-user
            // aggregate cap. Writes go through the Cosmic Settings UI,
            // not the CLI; this is read-only.
            let user_sub = args.get(1).map(String::as_str).unwrap_or("show");
            match user_sub {
                "show" | "" => {
                    let cfg = user_budget::load()?;
                    let store = budget::Store::open()?;
                    let snap = store
                        .current(user_budget::USER_BUDGET_BUCKET)
                        .map_err(|e| e.to_string())?;
                    let cap = cfg.monthly_units;
                    let used = snap.units_used;
                    let available = if cap == 0 {
                        None
                    } else if used >= cap {
                        Some(0u64)
                    } else {
                        Some(cap - used)
                    };
                    Ok(json!({
                        "scope": "user",
                        "path": user_budget::config_path().display().to_string(),
                        "period": snap.period,
                        "units_used": used,
                        "units_cap": cap,
                        "unlimited": cap == 0,
                        "units_available": available,
                    }))
                }
                "path" => Ok(json!({
                    "scope": "user",
                    "path": user_budget::config_path().display().to_string(),
                })),
                other => Err(format!(
                    "unknown subcommand: cos agent budget user {other}. try: show | path"
                )),
            }
        }
        _ => Err("usage: cos agent budget <show|reset|history> <app>  |  \
             cos agent budget user <show|path>"
            .to_string()),
    }
}

/// `cos agent override <show|path|effective> <app>` — read-only
/// inspection of the per-user override file at
/// `$HOME/.config/cos/apps/<app>.json`. There is no `set` / `write`
/// subcommand by design: the Cosmic Settings UI is the sole writer.
pub(super) fn override_cmd(args: &[String]) -> Result<Value, String> {
    use crate::ai::overrides;

    let sub = args.first().map(String::as_str).unwrap_or("");
    match sub {
        "show" => {
            let app = args
                .get(1)
                .ok_or_else(|| "usage: cos agent override show <app>".to_string())?;
            let ovr = overrides::load(app)?;
            Ok(json!({
                "app": app,
                "path": overrides::override_path(app).display().to_string(),
                "present": ovr.is_some(),
                "override": ovr,
            }))
        }
        "path" => {
            let app = args
                .get(1)
                .ok_or_else(|| "usage: cos agent override path <app>".to_string())?;
            Ok(json!({
                "app": app,
                "path": overrides::override_path(app).display().to_string(),
            }))
        }
        "effective" => {
            let app = args
                .get(1)
                .ok_or_else(|| "usage: cos agent override effective <app>".to_string())?;
            let apps_dir = std::env::var("COS_APPS_DIR")
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|_| std::path::PathBuf::from("/usr/lib/cos/apps"));
            let installed = apps::discover(&apps_dir)
                .get(app)
                .cloned()
                .ok_or_else(|| format!("unknown app `{app}`"))?;
            let manifest_policy =
                installed.manifest.ai.as_ref().ok_or_else(|| {
                    format!("app `{app}` has no `ai` block — nothing to override")
                })?;
            let ovr = overrides::load(app)?;
            let disabled = ovr.as_ref().map(|o| o.disabled).unwrap_or(false);
            let effective = overrides::apply_to_policy(manifest_policy, ovr.as_ref());
            Ok(json!({
                "app": app,
                "disabled": disabled,
                "manifest": manifest_policy,
                "override": ovr,
                "effective": effective,
            }))
        }
        _ => Err("usage: cos agent override <show|path|effective> <app>".to_string()),
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/app_ai_commands.rs"
    ));
}
