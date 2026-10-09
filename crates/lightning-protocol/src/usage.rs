use serde::{Deserialize, Serialize};

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
    fn finish_reason_serializes_snake_case() {
        assert_eq!(serde_json::to_string(&FinishReason::ToolUse).unwrap(), "\"tool_use\"");
    }
}
