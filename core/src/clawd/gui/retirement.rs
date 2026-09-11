use std::time::Instant;

use crate::approvals::RevocationScope;

pub(crate) async fn wait(
    deadline: Instant,
    retire: impl FnOnce(Instant) -> Result<(), String> + Send + 'static,
) -> Result<(), String> {
    let completion = tokio::task::spawn_blocking(move || retire(deadline));
    // A timed-out acknowledgement must not cancel the cleanup holding custody.
    tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), completion)
        .await
        .map_err(|_| "GUI retirement deadline exceeded; teardown remains pending".to_string())?
        .map_err(|error| format!("GUI retirement worker failed: {error}"))?
}

pub(super) fn approval_matches(
    scope: &RevocationScope,
    owner: u32,
    belongs_to_session: impl FnOnce(&str) -> bool,
) -> bool {
    if scope.owner_uid() != Some(owner) {
        return false;
    }
    match scope {
        RevocationScope::Owner { .. } => true,
        RevocationScope::Session { session, .. } => belongs_to_session(session),
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/gui/retirement.rs"
    ));
}
