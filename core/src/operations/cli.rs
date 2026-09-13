//! Thin terminal client for the shared, non-executing preview service.

use serde_json::{json, Value};

use crate::clawd::{client, config, protocol::Request, routes::Command};

pub fn run(command: &str, args: &[String]) -> Result<Value, String> {
    let (route, params) = parse(command, args)?;
    request(route, params)
}

pub(crate) fn request(route: Command, params: Value) -> Result<Value, String> {
    let response = client::request_blocking(config::socket_path(), Request::build(route, params))?;
    if !response.ok {
        return Err(response
            .error
            .map(|error| format!("{}: {}", error.code, error.message))
            .unwrap_or_else(|| "operation request was refused without an error".into()));
    }
    response
        .result
        .ok_or_else(|| "operation request returned no result".into())
}

fn parse(command: &str, args: &[String]) -> Result<(Command, Value), String> {
    if command != "preview" || args.len() < 2 {
        return Err(
            "usage: cos operation preview APP OPERATION [--activity ID] -- [APP ARGS]".into(),
        );
    }
    let mut params = json!({"app_id":args[0], "operation":args[1]});
    let mut index = 2;
    let mut route = Command::OperationPreview;
    if args.get(index).is_some_and(|arg| arg == "--activity") {
        let id = args.get(index + 1).ok_or("--activity requires an ID")?;
        crate::activities::validate_id(id).map_err(|error| error.to_string())?;
        params["id"] = json!(id);
        route = Command::ActivityOperationPreview;
        index += 2;
    }
    if args.get(index).is_some_and(|arg| arg != "--") {
        return Err("use -- before the App operation's arguments".into());
    }
    if args.get(index).is_some() {
        index += 1;
    }
    params["args"] = json!(&args[index..]);
    let params = (route.route().decode)(params)
        .map_err(|error| format!("invalid operation preview: {}", error.message()))?;
    Ok((route, params))
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/operations/cli.rs"
    ));
}
