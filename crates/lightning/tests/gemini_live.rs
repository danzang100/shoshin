//! Live Gemini smoke tests. Run manually with a key in `crates/lightning/.env`:
//! `cargo test -p lightning --test gemini_live -- --ignored --nocapture`
//! Uses only gemini-3.5-flash-lite.
use lightning::llm::{FinishReason, GeminiClient, LlmClient, LlmRequest};
use lightning::llm::gemini::DEFAULT_MODEL;
use lightning::tools::registry::tool_specs;
use lightning::turn::{ToolOutcome, ToolResult, Turn};

fn client() -> GeminiClient {
    dotenv::dotenv().ok();
    GeminiClient::from_env(DEFAULT_MODEL).expect("GEMINI_API_KEY must be set")
}

fn request(turns: Vec<Turn>, with_tools: bool) -> LlmRequest {
    LlmRequest {
        system: "You are a coding assistant. Use the tools instead of guessing.".into(),
        turns,
        tools: if with_tools { tool_specs() } else { vec![] },
        max_output_tokens: Some(512),
    }
}

#[tokio::test]
#[ignore = "calls the live Gemini API"]
async fn plain_text_round_trip() {
    let turns = vec![Turn::User { text: "Reply with the single word: pong".into() }];
    let res = client().complete(request(turns, false)).await.unwrap();
    assert_eq!(res.finish, FinishReason::Stop);
    let Turn::Assistant { text, .. } = res.turn else { panic!() };
    assert!(text.unwrap().to_lowercase().contains("pong"));
}

#[tokio::test]
#[ignore = "calls the live Gemini API"]
async fn tool_call_then_result_then_answer() {
    let c = client();
    let mut turns = vec![Turn::User { text: "List the files in the workspace root.".into() }];
    let first = c.complete(request(turns.clone(), true)).await.unwrap();
    assert_eq!(first.finish, FinishReason::ToolUse);
    let uses = first.turn.tool_uses().to_vec();
    assert_eq!(uses[0].name, "list_dir");
    turns.push(first.turn);
    turns.push(Turn::Tool {
        results: vec![ToolResult {
            tool_use_id: uses[0].id.clone(),
            name: uses[0].name.clone(),
            outcome: ToolOutcome::Ok { content: "Cargo.toml\nsrc/\n".into(), metadata: serde_json::json!({}), truncated: false },
        }],
    });
    let second = c.complete(request(turns, true)).await.unwrap();
    assert_eq!(second.finish, FinishReason::Stop);
    let Turn::Assistant { text, .. } = second.turn else { panic!() };
    assert!(text.unwrap().contains("Cargo.toml"));
}

#[tokio::test]
#[ignore = "calls the live Gemini API"]
async fn bad_key_is_classified_as_auth() {
    let c = GeminiClient::new("not-a-real-key", DEFAULT_MODEL);
    let err = c.complete(request(vec![Turn::User { text: "hi".into() }], false)).await.unwrap_err();
    assert_eq!(err, lightning::llm::LlmError::Auth);
}
