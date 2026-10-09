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

mod session {
    use std::sync::Arc;

    use lightning::llm::GeminiClient;
    use lightning::llm::gemini::DEFAULT_MODEL;
    use lightning::obs::{EventKind, MemorySink, Recorder, Redactor, SessionId, SessionStatus};
    use lightning::session::{Session, SessionConfig};
    use lightning::tools::dispatch::{Dispatcher, ToolLimits};
    use lightning::tools::gate::{FixedPrompter, Gate, PermissionMode};
    use lightning::tools::workspace::Workspace;
    use lightning::turn::Turn;

    fn live_session(dir: &std::path::Path, mode: PermissionMode, approve: bool) -> (Session<GeminiClient>, MemorySink) {
        dotenv::dotenv().ok();
        let sink = MemorySink::default();
        let rec = Recorder::new(SessionId::new(), Redactor::from_env(), vec![Box::new(sink.clone())]);
        let gate = Gate::new(mode, Arc::new(FixedPrompter(approve)));
        let dispatcher = Dispatcher::new(Workspace::new(dir).unwrap(), ToolLimits::default(), gate);
        let llm = GeminiClient::from_env(DEFAULT_MODEL).expect("GEMINI_API_KEY must be set");
        (Session::new(llm, dispatcher, rec, SessionConfig::new("gemini", DEFAULT_MODEL)), sink)
    }

    fn temp_dir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("lightning-live-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn last_text(session: &Session<GeminiClient>) -> String {
        match session.history().last() {
            Some(Turn::Assistant { text, .. }) => text.clone().unwrap_or_default(),
            other => panic!("expected assistant turn, got {other:?}"),
        }
    }

    /// Scenario 5: the model sees earlier turns of the same session.
    #[tokio::test]
    #[ignore = "calls the live Gemini API"]
    async fn follow_up_uses_earlier_turns() {
        let dir = temp_dir();
        let (mut session, sink) = live_session(&dir, PermissionMode::Ask, false);
        assert_eq!(session.run_turn("My lucky number is 4217. Just acknowledge it.").await, SessionStatus::Completed);
        assert_eq!(session.run_turn("What was my lucky number? Answer with the number only.").await, SessionStatus::Completed);
        assert!(last_text(&session).contains("4217"), "got: {}", last_text(&session));
        session.end(SessionStatus::Completed);
        let users = sink.events().iter().filter(|e| matches!(e.kind, EventKind::UserMessage { .. })).count();
        assert_eq!(users, 2);
        std::fs::remove_dir_all(dir).ok();
    }

    /// Scenario 2: a failing tool call is recoverable.
    #[tokio::test]
    #[ignore = "calls the live Gemini API"]
    async fn missing_file_is_recoverable() {
        let dir = temp_dir();
        let (mut session, sink) = live_session(&dir, PermissionMode::Ask, false);
        assert_eq!(session.run("Read the file nothing_here.txt and tell me its contents.").await, SessionStatus::Completed);
        let errors = sink
            .events()
            .iter()
            .filter(|e| matches!(&e.kind, EventKind::ToolFinished { error_class: Some(c), .. } if c == "not_found"))
            .count();
        assert!(errors >= 1);
        std::fs::remove_dir_all(dir).ok();
    }

    /// Scenario 9: approve writes the file, deny leaves it absent, both complete.
    #[tokio::test]
    #[ignore = "calls the live Gemini API"]
    async fn write_file_through_the_gate() {
        let task = "Create the file notes.txt containing the single word: hello";

        let approved = temp_dir();
        let (mut session, _) = live_session(&approved, PermissionMode::Ask, true);
        assert_eq!(session.run(task).await, SessionStatus::Completed);
        assert!(std::fs::read_to_string(approved.join("notes.txt")).unwrap().to_lowercase().contains("hello"));

        let denied = temp_dir();
        let (mut session, _) = live_session(&denied, PermissionMode::Ask, false);
        assert_eq!(session.run(task).await, SessionStatus::Completed);
        assert!(!denied.join("notes.txt").exists());

        std::fs::remove_dir_all(approved).ok();
        std::fs::remove_dir_all(denied).ok();
    }
}
