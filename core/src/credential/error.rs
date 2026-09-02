use std::error::Error;
use std::fmt;
use std::path::Path;

/// Stable category for failures owned by the credential subsystem.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CredentialErrorKind {
    InvalidInput,
    NotFound,
    NotAuthorized,
    Unavailable,
    Corrupt,
    External,
}

/// Credential failure with an operation name and an optional underlying
/// source. The rendered message intentionally never contains credential
/// values.
pub struct CredentialError {
    kind: CredentialErrorKind,
    operation: &'static str,
    message: String,
    source: Option<Box<dyn Error + Send + Sync>>,
}

impl CredentialError {
    pub fn kind(&self) -> CredentialErrorKind {
        self.kind
    }

    pub fn operation(&self) -> &'static str {
        self.operation
    }

    pub fn contains(&self, pattern: &str) -> bool {
        self.message.contains(pattern)
    }

    pub(crate) fn context(self, operation: &'static str, message: impl Into<String>) -> Self {
        let kind = self.kind;
        let message = format!("{}: {self}", message.into());
        Self::with_source(kind, operation, message, self)
    }

    pub(crate) fn invalid(operation: &'static str, message: impl Into<String>) -> Self {
        Self::message(CredentialErrorKind::InvalidInput, operation, message)
    }

    pub(crate) fn not_found(operation: &'static str, message: impl Into<String>) -> Self {
        Self::message(CredentialErrorKind::NotFound, operation, message)
    }

    pub(crate) fn unauthorized(operation: &'static str, message: impl Into<String>) -> Self {
        Self::message(CredentialErrorKind::NotAuthorized, operation, message)
    }

    pub(crate) fn unavailable(operation: &'static str, message: impl Into<String>) -> Self {
        Self::message(CredentialErrorKind::Unavailable, operation, message)
    }

    pub(crate) fn corrupt(operation: &'static str, message: impl Into<String>) -> Self {
        Self::message(CredentialErrorKind::Corrupt, operation, message)
    }

    pub fn external(operation: &'static str, message: impl Into<String>) -> Self {
        Self::message(
            CredentialErrorKind::External,
            operation,
            redact_token_like(&message.into()),
        )
    }

    pub(crate) fn io(
        operation: &'static str,
        context: impl Into<String>,
        source: std::io::Error,
    ) -> Self {
        let context = context.into();
        Self {
            kind: CredentialErrorKind::Unavailable,
            operation,
            message: format!("{context}: {source}"),
            source: Some(Box::new(source)),
        }
    }

    pub(crate) fn io_at(
        operation: &'static str,
        action: &'static str,
        path: &Path,
        source: std::io::Error,
    ) -> Self {
        Self::io(operation, format!("{action} {}", path.display()), source)
    }

    pub(crate) fn with_source<E>(
        kind: CredentialErrorKind,
        operation: &'static str,
        message: impl Into<String>,
        source: E,
    ) -> Self
    where
        E: Error + Send + Sync + 'static,
    {
        Self {
            kind,
            operation,
            message: message.into(),
            source: Some(Box::new(source)),
        }
    }

    fn message(
        kind: CredentialErrorKind,
        operation: &'static str,
        message: impl Into<String>,
    ) -> Self {
        Self {
            kind,
            operation,
            message: message.into(),
            source: None,
        }
    }
}

impl fmt::Debug for CredentialError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CredentialError")
            .field("kind", &self.kind)
            .field("operation", &self.operation)
            .field("has_source", &self.source.is_some())
            .finish()
    }
}

impl fmt::Display for CredentialError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl Error for CredentialError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.source
            .as_deref()
            .map(|source| source as &(dyn Error + 'static))
    }
}

pub type CredentialResult<T> = Result<T, CredentialError>;

fn redact_token_like(message: &str) -> String {
    let mut output = String::with_capacity(message.len());
    let mut token = String::new();
    for character in message.chars() {
        if character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.') {
            token.push(character);
            continue;
        }
        push_redacted_token(&mut output, &token);
        token.clear();
        output.push(character);
    }
    push_redacted_token(&mut output, &token);
    output
}

fn push_redacted_token(output: &mut String, token: &str) {
    if token.len() >= 24 {
        output.extend(token.chars().take(4));
        output.push_str("***");
        let suffix = token.chars().rev().take(4).collect::<Vec<_>>();
        output.extend(suffix.into_iter().rev());
    } else {
        output.push_str(token);
    }
}
