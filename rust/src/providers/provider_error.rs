use serde::{Deserialize, Serialize};

/// Stable provider-failure classification carried across the host/kernel boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderErrorKind {
    Transport,
    Auth,
    RateLimit,
    ContextOverflow,
    InvalidRequest,
    Modality,
    ModelUnavailable,
    Protocol,
    Unknown,
}

impl ProviderErrorKind {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Transport => "transport",
            Self::Auth => "auth",
            Self::RateLimit => "rate_limit",
            Self::ContextOverflow => "context_overflow",
            Self::InvalidRequest => "invalid_request",
            Self::Modality => "modality",
            Self::ModelUnavailable => "model_unavailable",
            Self::Protocol => "protocol",
            Self::Unknown => "unknown",
        }
    }
}

/// Structured provider error. Only these scalar fields cross into the canonical host event.
#[derive(Debug, Clone, thiserror::Error)]
#[error("{message}")]
pub struct ProviderError {
    pub provider: String,
    pub kind: ProviderErrorKind,
    pub retryable: bool,
    pub message: String,
    pub http_status: Option<u16>,
    pub provider_code: Option<String>,
}

impl ProviderError {
    pub fn new(
        provider: impl Into<String>,
        kind: ProviderErrorKind,
        retryable: bool,
        message: impl Into<String>,
    ) -> Self {
        Self {
            provider: provider.into(),
            kind,
            retryable,
            message: message.into(),
            http_status: None,
            provider_code: None,
        }
    }

    pub fn transport(provider: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(provider, ProviderErrorKind::Transport, true, message)
    }

    pub fn from_http(provider: impl Into<String>, status: u16, body: impl Into<String>) -> Self {
        let provider = provider.into();
        let body = body.into();
        let provider_code = provider_code(&body);
        let kind = classify_http(status, provider_code.as_deref());
        let retryable = matches!(
            kind,
            ProviderErrorKind::Transport
                | ProviderErrorKind::RateLimit
                | ProviderErrorKind::ModelUnavailable
        );
        Self {
            message: format!("{provider} HTTP {status}: {body}"),
            provider,
            kind,
            retryable,
            http_status: Some(status),
            provider_code,
        }
    }
}

fn provider_code(body: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    [
        value.pointer("/error/code"),
        value.pointer("/error/error_code"),
        value.pointer("/error/type"),
        value.get("code"),
        value.get("error_code"),
    ]
    .into_iter()
    .flatten()
    .find_map(|value| value.as_str().filter(|value| !value.is_empty()))
    .map(str::to_owned)
}

fn classify_http(status: u16, provider_code: Option<&str>) -> ProviderErrorKind {
    if status == 413
        || provider_code.is_some_and(|code| {
            code.eq_ignore_ascii_case("context_length_exceeded")
                || code.eq_ignore_ascii_case("prompt_too_long")
        })
    {
        return ProviderErrorKind::ContextOverflow;
    }
    match status {
        401 | 403 => ProviderErrorKind::Auth,
        429 => ProviderErrorKind::RateLimit,
        404 | 500..=599 => ProviderErrorKind::ModelUnavailable,
        408 | 409 => ProviderErrorKind::Transport,
        400 | 422 => ProviderErrorKind::InvalidRequest,
        _ => ProviderErrorKind::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http_context_overflow_uses_status_or_structured_code() {
        let by_status = ProviderError::from_http("test", 413, "too large");
        assert_eq!(by_status.kind, ProviderErrorKind::ContextOverflow);

        let by_code = ProviderError::from_http(
            "test",
            400,
            r#"{"error":{"code":"context_length_exceeded"}}"#,
        );
        assert_eq!(by_code.kind, ProviderErrorKind::ContextOverflow);
        assert_eq!(
            by_code.provider_code.as_deref(),
            Some("context_length_exceeded")
        );
    }

    #[test]
    fn prose_does_not_determine_failure_kind() {
        let error = ProviderError::from_http("test", 400, "413 prompt too long");
        assert_eq!(error.kind, ProviderErrorKind::InvalidRequest);
    }
}
