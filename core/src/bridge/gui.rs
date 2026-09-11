use std::collections::BTreeMap;
use std::path::Path;

use claw_display_control::{Epoch, InstanceId};

use crate::caps::{manifest::Runtime, CapSet};
use crate::clawd::gui::inputs::Presentation;
use crate::worker::{StdioPlan, WorkerLaunch};

use super::{gui_args, AppLaunch, LaunchBindingRef};

pub(crate) fn launch(
    launch: &AppLaunch,
    selector: &str,
    args: &[String],
    data_dir: &str,
) -> Result<(), String> {
    use crate::clawd::gui::inputs::{LaunchReply, PresentationKey, StopReply, WaitReply};
    use std::io::Write;

    gui_args::prepare(launch.manifest().runtime, selector, args)?;
    if !super::use_clawd_app_session_backend()? {
        return Err("GUI launch requires an authenticated login and Root GUI custody; local execution is unavailable".to_string());
    }
    let mut presentation = Presentation::new();
    for key in PresentationKey::ALL {
        match std::env::var(key.as_str()) {
            Ok(value) => {
                let value = crate::clawd::wire::bounded::Text::parse(&value)
                    .map_err(|error| format!("invalid GUI presentation value: {error}"))?;
                presentation.insert(key, value);
            }
            Err(std::env::VarError::NotPresent) => {}
            Err(std::env::VarError::NotUnicode(_)) => {
                return Err("GUI presentation must be UTF-8".to_string())
            }
        }
    }
    let mut session = super::AppIdentitySession::for_gui(launch, launch.app_id(), selector)?;
    let result = (|| {
        let expected = session
            .gui_data_dir
            .as_ref()
            .ok_or("OS broker did not bind a GUI data directory")?;
        if Path::new(data_dir) != expected {
            return Err("GUI data directory differs from the OS owner binding; caller-owned path overrides are not supported".to_string());
        }
        let super::AppSessionBackend::Clawd { handle, .. } = &session.backend else {
            return Err("GUI launch has no authenticated Root registration".to_string());
        };
        let started = super::clawd_request(super::ClawdCommand::AppGuiLaunch, serde_json::json!({
            "session_id": session.id(), "handle": handle, "args": args, "presentation": presentation,
        })).map_err(String::from)?;
        let started: LaunchReply = serde_json::from_value(started)
            .map_err(|error| format!("invalid GUI launch response: {error}"))?;
        if !started.started {
            return Err("Root GUI launch was not acknowledged".to_string());
        }
        loop {
            let response = super::clawd_request(
                super::ClawdCommand::AppGuiWait,
                serde_json::json!({ "session_id": session.id() }),
            )
            .map_err(String::from)?;
            let response: WaitReply = serde_json::from_value(response)
                .map_err(|error| format!("invalid GUI lifecycle response: {error}"))?;
            if response.finished {
                let outcome = response
                    .outcome
                    .ok_or("GUI completion omitted its checked outcome")?;
                session.gui_retired = true;
                std::io::stdout()
                    .write_all(outcome.stdout.text.as_bytes())
                    .map_err(|error| format!("write GUI output: {error}"))?;
                std::io::stderr()
                    .write_all(outcome.stderr.text.as_bytes())
                    .map_err(|error| format!("write GUI diagnostics: {error}"))?;
                if outcome.stdout.truncated || outcome.stderr.truncated {
                    eprintln!("GUI diagnostics retained only the last 64 KiB per stream");
                }
                if let Some(error) = outcome.error {
                    return Err(error);
                }
                if outcome.exit_code != Some(0) {
                    return Err(format!(
                        "GUI `{}` exited without success ({:?})",
                        launch.app_id(),
                        outcome.exit_code
                    ));
                }
                return Ok(());
            }
            if response.retiring == Some(true) {
                if let Some(error) = response.retirement_error {
                    return Err(format!("GUI retirement is incomplete: {error}"));
                }
            }
        }
    })();
    if !session.gui_retired {
        let cleanup = super::clawd_request(
            super::ClawdCommand::AppGuiStop,
            serde_json::json!({ "session_id": session.id() }),
        )
        .map_err(String::from)
        .and_then(|value| {
            serde_json::from_value::<StopReply>(value)
                .map_err(|error| format!("invalid GUI retirement response: {error}"))
        })
        .and_then(|reply| {
            if reply.retired {
                Ok(())
            } else {
                Err("GUI retirement was not confirmed".to_string())
            }
        });
        match cleanup {
            Ok(()) => session.gui_retired = true,
            Err(error) => {
                return Err(format!(
                    "{}; {error}",
                    result
                        .err()
                        .unwrap_or_else(|| "GUI launch stopped".to_string())
                ))
            }
        }
    }
    result
}

pub(crate) struct PreparedPlan {
    pub launch: WorkerLaunch,
    pub binding: LaunchBindingRef,
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn plan(
    launch: &AppLaunch,
    selector: &str,
    args: &[String],
    presentation: &Presentation,
    session: &str,
    caps: &CapSet,
    instance: InstanceId,
    display: &Path,
) -> Result<PreparedPlan, String> {
    let manifest = launch.manifest();
    let runtime = manifest.runtime;
    let arguments = gui_args::prepare(runtime, selector, args)?;
    let entry = manifest
        .entry
        .clone()
        .unwrap_or_else(|| runtime.default_entry().to_string());
    let binding = launch.bind(std::slice::from_ref(&entry))?;
    let data = crate::paths::user_data_dir();
    let apps = launch
        .dir()
        .parent()
        .ok_or("GUI package has no parent directory")?;
    let data = data.to_str().ok_or("GUI data directory is not UTF-8")?;
    let apps = apps.to_str().ok_or("GUI package parent is not UTF-8")?;
    let (program, mut argv) = if matches!(runtime, Runtime::Python) {
        let wrapper =
            super::python_wrapper(&launch.dir().join("main.py"), selector, args, data, apps)?;
        (
            super::interpreter_path("python3")?,
            vec!["-c".to_string(), wrapper],
        )
    } else {
        super::session_program(runtime, &launch.dir().join(&entry))?
    };
    argv.extend(arguments.entry_argv);
    let app_runner = super::app_runner_path();
    let runner = app_runner.with_file_name("claw-gui-runner");
    for executable in [&runner, &app_runner] {
        crate::display_session::protected_executable(
            executable.to_str().ok_or("GUI runner path is not UTF-8")?,
        )
        .map_err(|error| error.to_string())?;
    }
    let mut gated = vec![
        "--launch-gate".to_string(),
        Epoch(instance.0).label(),
        "--".to_string(),
        program
            .to_str()
            .ok_or("GUI program path is not UTF-8")?
            .to_string(),
    ];
    gated.extend(argv);
    let panel = manifest
        .desktop
        .as_ref()
        .is_some_and(|desktop| desktop.panel_applet);
    let mut environment = BTreeMap::from([
        ("COS_APP_GUI".to_string(), "1".to_string()),
        ("COS_COMMAND".to_string(), selector.to_string()),
        ("COS_ARGS_JSON".to_string(), arguments.user_args_json),
    ]);
    for (key, value) in presentation {
        if panel || !key.is_panel() {
            environment.insert(key.as_str().to_string(), value.as_str().to_string());
        }
    }
    let mut policy = crate::worker::derive::gui_operation(
        crate::worker::derive::AppOperationInput {
            app_id: launch.app_id(),
            app_dir: launch.dir(),
            operation: selector,
            program: runner,
            argv: gated,
            caps,
            session_id: session,
            data_dir: data,
            apps_dir: apps,
            extra_env: environment,
            stdio: StdioPlan::Captured,
            desktop: false,
            package_identity: binding.dir_identity(),
            pinned_entries: binding.entries(),
            developer: binding.is_developer(),
        },
        display,
        &program,
    )?;
    if !presentation.contains_key(&crate::clawd::gui::inputs::PresentationKey::LocaleAll) {
        policy.env.remove("LC_ALL");
    }
    policy
        .mounts
        .extend(crate::worker::derive::program_mount(&app_runner));
    Ok(PreparedPlan {
        launch: WorkerLaunch::new(policy),
        binding,
    })
}
