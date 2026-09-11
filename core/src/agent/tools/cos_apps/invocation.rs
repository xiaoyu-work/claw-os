//! One ordinary App invocation followed by optional, reporting-only capture.

use crate::bridge::AppLaunch;
use crate::operations::{receipts, reporting};

use super::super::ToolResult;

pub(super) async fn run(
    launch: AppLaunch,
    command: String,
    args: Vec<String>,
    data: String,
    apps: String,
) -> ToolResult {
    let executed_launch = launch.clone();
    let executed_command = command.clone();
    let invoke =
        move || crate::bridge::run_app(&executed_launch, &executed_command, &args, &data, &apps);
    let result =
        if crate::paths::is_routed_job() || crate::paths::current_owner_uid_override().is_some() {
            tokio::task::block_in_place(invoke)
        } else {
            match tokio::task::spawn_blocking(invoke).await {
                Ok(result) => result,
                Err(error) => Err(format!("cos app bridge panicked: {error}")),
            }
        };
    finish(&launch, &command, result).await
}

async fn finish(
    launch: &AppLaunch,
    command: &str,
    result: Result<Option<String>, String>,
) -> ToolResult {
    let report = reporting::current()
        .filter(|_| launch.manifest().operations.contains_key(command))
        .map(|recorder| {
            let report = receipts::capture(
                uuid::Uuid::new_v4().to_string(),
                launch.app_id().to_string(),
                command.to_string(),
                launch.package().content_digest().to_string(),
                result.clone(),
            );
            (recorder, report)
        });
    let output = match result {
        Ok(Some(text)) => ToolResult::ok(text),
        Ok(None) => ToolResult::ok(String::new()),
        Err(message) => ToolResult::err(message),
    };
    let Some((recorder, report)) = report else {
        return output;
    };
    super::super::app_receipts::deliver(output, recorder, report).await
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/tools/cos_apps/invocation.rs"
    ));
}
