//! Provider-neutral LLM contract (spec 001, section 6).
//!
//! Adapters (Gemini first) implement [`LlmClient`]; the session and the tests only
//! talk to the trait. [`FakeLlm`] is the scripted test double.
use std::fmt;
use std::future::Future;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::tools::registry::ToolSpec;
use crate::turn::Turn;

// pub mod openai;  // stale draft, rewritten in a later task
mod fake;
pub mod gemini;

pub use fake::FakeLlm;
pub use gemini::GeminiClient;

#[derive(Debug, Clone)]
pub struct LlmRequest {
    pub system: String,
    pub turns: Vec<Turn>,
    pub tools: Vec<ToolSpec>,
    pub max_output_tokens: Option<u32>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LlmResponse {
    /// Always a `Turn::Assistant`.
    pub turn: Turn,
    pub usage: Usage,
    pub finish: FinishReason,
    pub latency_ms: u64,
    /// Full request/response bodies, present only when the adapter was asked to
    /// capture them (debug logging).
    pub exchange: Option<Exchange>,
}

/// Provider request and response bodies for one call.
#[derive(Debug, Clone, PartialEq)]
pub struct Exchange {
    pub request: serde_json::Value,
    pub response: serde_json::Value,
}

/// Token counts. Every field is optional because providers differ and the Gemini
/// `total_*` fields are unreliable (see docs/gemini-notes.md).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub thinking_tokens: Option<u64>,
}

impl Usage {
    /// Adds `other` into `self`, treating a missing value as zero unless both are missing.
    pub fn add(&mut self, other: &Usage) {
        fn sum(a: Option<u64>, b: Option<u64>) -> Option<u64> {
            match (a, b) {
                (None, None) => None,
                (a, b) => Some(a.unwrap_or(0) + b.unwrap_or(0)),
            }
        }
        self.input_tokens = sum(self.input_tokens, other.input_tokens);
        self.output_tokens = sum(self.output_tokens, other.output_tokens);
        self.thinking_tokens = sum(self.thinking_tokens, other.thinking_tokens);
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    Stop,
    ToolUse,
    MaxTokens,
    Safety,
    Other(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum LlmError {
    /// DNS, connect or timeout failure.
    Transport(String),
    /// Non-success HTTP status. `body` is kept for logs and must already be free of keys.
    Http { status: u16, body: String },
    RateLimited { retry_after: Option<Duration> },
    /// Bad or missing credentials. Never retried.
    Auth,
    /// Unparseable or empty response.
    BadResponse(String),
}

impl LlmError {
    /// Whether the retry policy (spec section 6.4) may try again.
    pub fn is_retryable(&self) -> bool {
        match self {
            LlmError::Transport(_) | LlmError::RateLimited { .. } => true,
            LlmError::Http { status, .. } => *status >= 500,
            LlmError::Auth | LlmError::BadResponse(_) => false,
        }
    }
}

impl fmt::Display for LlmError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LlmError::Transport(msg) => write!(f, "network error: {msg}"),
            LlmError::Http { status, body } => write!(f, "HTTP {status}: {body}"),
            LlmError::RateLimited { retry_after: Some(d) } => {
                write!(f, "rate limited, retry after {}s", d.as_secs())
            }
            LlmError::RateLimited { retry_after: None } => write!(f, "rate limited"),
            LlmError::Auth => write!(f, "authentication failed (check the API key)"),
            LlmError::BadResponse(msg) => write!(f, "bad response from provider: {msg}"),
        }
    }
}

impl std::error::Error for LlmError {}

/// The single LLM call seam. Adapters own retries' *inputs* (they classify errors);
/// the retry loop itself lives in the session.
pub trait LlmClient {
    fn complete(&self, req: LlmRequest) -> impl Future<Output = Result<LlmResponse, LlmError>> + Send;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_add_treats_missing_as_zero_unless_both_missing() {
        let mut total = Usage::default();
        total.add(&Usage { input_tokens: Some(10), output_tokens: None, thinking_tokens: None });
        total.add(&Usage { input_tokens: Some(5), output_tokens: Some(3), thinking_tokens: None });
        assert_eq!(total, Usage { input_tokens: Some(15), output_tokens: Some(3), thinking_tokens: None });
    }

    #[test]
    fn retryable_matches_policy() {
        assert!(LlmError::Transport("x".into()).is_retryable());
        assert!(LlmError::RateLimited { retry_after: None }.is_retryable());
        assert!(LlmError::Http { status: 503, body: String::new() }.is_retryable());
        assert!(!LlmError::Http { status: 400, body: String::new() }.is_retryable());
        assert!(!LlmError::Auth.is_retryable());
        assert!(!LlmError::BadResponse("x".into()).is_retryable());
    }
}
