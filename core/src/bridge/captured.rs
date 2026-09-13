//! Bind-gated commands for captured ordinary App operations.

use std::path::PathBuf;

pub(super) struct GatedCommand {
    pub runner: PathBuf,
    pub program: PathBuf,
    pub argv: Vec<String>,
    pub token: [u8; 32],
}

impl GatedCommand {
    pub(super) fn new(program: PathBuf, arguments: Vec<String>) -> Result<Self, String> {
        let program_arg = program.to_str().ok_or("App program path is not UTF-8")?;
        let runner = super::app_runner_path()
            .canonicalize()
            .map_err(|error| format!("resolve App launch-gate runner: {error}"))?;
        let mut token = [0_u8; 32];
        let encoded = uuid::Uuid::new_v4()
            .simple()
            .encode_lower(&mut token)
            .to_string();
        let mut argv = vec![
            "--launch-gate".to_string(),
            encoded,
            "--".to_string(),
            program_arg.to_string(),
        ];
        argv.extend(arguments);
        Ok(Self {
            runner,
            program,
            argv,
            token,
        })
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/bridge/captured.rs"
    ));
}
