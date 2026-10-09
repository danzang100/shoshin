//! Scripted [`LlmClient`] for tests: no network, deterministic.
use std::collections::VecDeque;
use std::sync::Mutex;

use super::{FinishReason, LlmClient, LlmError, LlmRequest, LlmResponse, Usage};
use crate::turn::{ToolUse, Turn};

pub struct FakeLlm {
    script: Mutex<VecDeque<Result<LlmResponse, LlmError>>>,
    requests: Mutex<Vec<LlmRequest>>,
}

impl FakeLlm {
    /// Responses are returned in order, one per `complete` call.
    pub fn new(script: impl IntoIterator<Item = Result<LlmResponse, LlmError>>) -> Self {
        FakeLlm { script: Mutex::new(script.into_iter().collect()), requests: Mutex::new(Vec::new()) }
    }

    /// Every request received so far, oldest first.
    pub fn requests(&self) -> Vec<LlmRequest> {
        self.requests.lock().unwrap().clone()
    }

    /// Scripted assistant reply with plain text and no tool calls.
    pub fn text(text: &str) -> Result<LlmResponse, LlmError> {
        Ok(response(Turn::Assistant { text: Some(text.into()), tool_uses: vec![], raw: vec![] }, FinishReason::Stop))
    }

    /// Scripted assistant reply that requests one tool call.
    pub fn tool_call(id: &str, name: &str, arguments: serde_json::Value) -> Result<LlmResponse, LlmError> {
        let tool_use = ToolUse { id: id.into(), name: name.into(), arguments };
        Ok(response(Turn::Assistant { text: None, tool_uses: vec![tool_use], raw: vec![] }, FinishReason::ToolUse))
    }
}

fn response(turn: Turn, finish: FinishReason) -> LlmResponse {
    LlmResponse { turn, usage: Usage::default(), finish, latency_ms: 0, exchange: None }
}

impl LlmClient for FakeLlm {
    async fn complete(&self, req: LlmRequest) -> Result<LlmResponse, LlmError> {
        self.requests.lock().unwrap().push(req);
        self.script
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| Err(LlmError::BadResponse("FakeLlm script exhausted".into())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn request(text: &str) -> LlmRequest {
        LlmRequest {
            system: "sys".into(),
            turns: vec![Turn::User { text: text.into() }],
            tools: vec![],
            max_output_tokens: None,
        }
    }

    #[tokio::test]
    async fn returns_script_in_order_and_records_requests() {
        let fake = FakeLlm::new([
            FakeLlm::tool_call("c1", "list_dir", json!({ "dir_path": "." })),
            FakeLlm::text("done"),
        ]);

        let first = fake.complete(request("one")).await.unwrap();
        assert_eq!(first.finish, FinishReason::ToolUse);
        assert_eq!(first.turn.tool_uses().len(), 1);

        let second = fake.complete(request("two")).await.unwrap();
        assert_eq!(second.finish, FinishReason::Stop);

        let seen = fake.requests();
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[1].turns, vec![Turn::User { text: "two".into() }]);
    }

    #[tokio::test]
    async fn scripted_errors_pass_through_and_exhaustion_is_an_error() {
        let fake = FakeLlm::new([Err(LlmError::Auth)]);
        assert_eq!(fake.complete(request("a")).await.unwrap_err(), LlmError::Auth);
        assert!(matches!(fake.complete(request("b")).await, Err(LlmError::BadResponse(_))));
    }
}
