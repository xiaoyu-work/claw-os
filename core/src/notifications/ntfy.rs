use async_trait::async_trait;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use std::time::Duration;

use super::{Notification, Severity};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NtfyTarget {
    pub server: String,
    pub topic: String,
    pub bearer_token: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum DeliveryError {
    #[error("invalid delivery target: {0}")]
    InvalidTarget(String),
    #[error("delivery transport failed")]
    Transport,
    #[error("delivery timed out")]
    Timeout,
    #[error("delivery endpoint returned HTTP {0}")]
    Http(u16),
}

impl DeliveryError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidTarget(_) => "invalid_target",
            Self::Transport => "transport",
            Self::Timeout => "timeout",
            Self::Http(status) if *status >= 500 => "remote_5xx",
            Self::Http(_) => "remote_4xx",
        }
    }
}

#[async_trait]
pub trait DeliveryAdapter: Send + Sync {
    type Target: Send + Sync;

    async fn deliver(
        &self,
        notification: &Notification,
        target: &Self::Target,
    ) -> Result<(), DeliveryError>;
}

#[derive(Clone)]
pub struct NtfyAdapter {
    client: reqwest::Client,
}

impl Default for NtfyAdapter {
    fn default() -> Self {
        Self {
            client: reqwest::Client::new(),
        }
    }
}

impl NtfyAdapter {
    pub fn new(client: reqwest::Client) -> Self {
        Self { client }
    }

    fn endpoint(target: &NtfyTarget) -> Result<url::Url, DeliveryError> {
        let mut url = url::Url::parse(&target.server)
            .map_err(|_| DeliveryError::InvalidTarget("server is not a URL".to_string()))?;
        if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
            return Err(DeliveryError::InvalidTarget(
                "server must be an absolute HTTP(S) URL".to_string(),
            ));
        }
        if target.topic.is_empty()
            || !target.topic.bytes().all(|byte| {
                byte.is_ascii_alphanumeric()
                    || matches!(byte, b'.' | b'_' | b'-' | b':' | b'@' | b'+')
            })
        {
            return Err(DeliveryError::InvalidTarget(
                "topic contains unsupported characters".to_string(),
            ));
        }
        url.set_query(None);
        url.set_fragment(None);
        url.path_segments_mut()
            .map_err(|_| DeliveryError::InvalidTarget("server cannot accept a topic".to_string()))?
            .pop_if_empty()
            .push(&target.topic);
        Ok(url)
    }
}

#[async_trait]
impl DeliveryAdapter for NtfyAdapter {
    type Target = NtfyTarget;

    async fn deliver(
        &self,
        notification: &Notification,
        target: &Self::Target,
    ) -> Result<(), DeliveryError> {
        let endpoint = Self::endpoint(target)?;
        let mut headers = HeaderMap::new();
        headers.insert(
            CONTENT_TYPE,
            HeaderValue::from_static("text/plain; charset=utf-8"),
        );
        headers.insert(
            "Title",
            HeaderValue::from_str(&notification.title).map_err(|_| {
                DeliveryError::InvalidTarget("title is not header-safe".to_string())
            })?,
        );
        headers.insert(
            "Priority",
            HeaderValue::from_static(match notification.severity {
                Severity::Info => "default",
                Severity::Warning => "high",
                Severity::Error | Severity::Critical => "max",
            }),
        );
        if let Some(action) = notification.actions.first() {
            headers.insert(
                "Click",
                HeaderValue::from_str(&action.uri).map_err(|_| {
                    DeliveryError::InvalidTarget("action URI is not header-safe".to_string())
                })?,
            );
        }
        if let Some(token) = target
            .bearer_token
            .as_deref()
            .filter(|token| !token.is_empty())
        {
            let value = HeaderValue::from_str(&format!("Bearer {token}")).map_err(|_| {
                DeliveryError::InvalidTarget("token is not header-safe".to_string())
            })?;
            headers.insert(AUTHORIZATION, value);
        }

        let response = tokio::time::timeout(
            Duration::from_secs(20),
            self.client
                .post(endpoint)
                .headers(headers)
                .body(notification.body.clone())
                .send(),
        )
        .await
        .map_err(|_| DeliveryError::Timeout)?
        .map_err(|_| DeliveryError::Transport)?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(DeliveryError::Http(response.status().as_u16()))
        }
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/notifications/ntfy.rs"
    ));
}
