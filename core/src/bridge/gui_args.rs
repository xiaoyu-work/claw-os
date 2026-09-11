//! Literal GUI arguments, validated before App session authorization.

use crate::caps::manifest::Runtime;
use crate::clawd::wire::bounded::{Text, MAX_STRUCTURED_STRING_BYTES};

const MAX_ARGUMENTS: usize = 128;
const ARGUMENT_BYTES: usize = 4096;
const SELECTOR_BYTES: usize = 1024;

pub(crate) struct GuiArguments {
    pub entry_argv: Vec<String>,
    pub user_args_json: String,
}

pub(crate) fn prepare(
    runtime: Runtime,
    selector: &str,
    args: &[String],
) -> Result<GuiArguments, String> {
    Text::<SELECTOR_BYTES>::parse(selector)
        .map_err(|error| format!("invalid GUI selector: {error}"))?;
    if selector.trim().is_empty() || selector.chars().any(char::is_control) {
        return Err("GUI selector must be nonempty and contain no control characters".to_string());
    }
    let user_args_json = validate_args(args)?;
    let entry_argv = match runtime {
        Runtime::Python => Vec::new(),
        Runtime::Binary | Runtime::Node | Runtime::Shell => std::iter::once(selector.to_string())
            .chain(args.iter().cloned())
            .collect(),
    };
    Ok(GuiArguments {
        entry_argv,
        user_args_json,
    })
}

pub(crate) fn validate_args(args: &[String]) -> Result<String, String> {
    if args.len() > MAX_ARGUMENTS {
        return Err(format!(
            "GUI launch accepts at most {MAX_ARGUMENTS} user arguments"
        ));
    }
    for (index, argument) in args.iter().enumerate() {
        Text::<ARGUMENT_BYTES>::parse(argument)
            .map_err(|error| format!("invalid GUI argument {}: {error}", index + 1))?;
    }
    let user_args_json =
        serde_json::to_string(args).map_err(|error| format!("serialize GUI arguments: {error}"))?;
    if user_args_json.len() > MAX_STRUCTURED_STRING_BYTES {
        return Err(format!(
            "encoded GUI arguments exceed {MAX_STRUCTURED_STRING_BYTES} bytes"
        ));
    }

    Ok(user_args_json)
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/bridge/gui_args.rs"
    ));
}
