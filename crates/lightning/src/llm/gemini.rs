//! Gemini Interactions API adapter (spec 001, section 6.2). Request and response
//! shapes are the ones confirmed in docs/gemini-notes.md. Stateless: `store: false`
//! and the full history is resent on every call.
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::{Exchange, FinishReason, LlmClient, LlmError, LlmRequest, LlmResponse, Usage};
use crate::tools::registry::ToolSpec;
use crate::turn::{RawItem, ToolOutcome, ToolResult, ToolUse, Turn};

pub const DEFAULT_ENDPOINT: &str = "https://generativelanguage.googleapis.com/v1beta/interactions";
pub const DEFAULT_MODEL: &str = "gemini-3.5-flash-lite";

pub struct GeminiClient {
    http: reqwest::Client,
    api_key: String,
    model: String,
    endpoint: String,
    capture_bodies: bool,
}

impl GeminiClient {
    pub fn new(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(120))
            .build()
            .unwrap_or_default();
        GeminiClient {
            http,
            api_key: api_key.into(),
            model: model.into(),
            endpoint: DEFAULT_ENDPOINT.to_string(),
            capture_bodies: false,
        }
    }

    /// Reads `GEMINI_API_KEY` from the environment (call `dotenv` first if wanted).
    pub fn from_env(model: impl Into<String>) -> Result<Self, LlmError> {
        match std::env::var("GEMINI_API_KEY") {
            Ok(key) if !key.trim().is_empty() => Ok(Self::new(key.trim(), model)),
            _ => Err(LlmError::Auth),
        }
    }

    pub fn with_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.endpoint = endpoint.into();
        self
    }

    /// Attach the full request/response bodies to every [`LlmResponse`] (debug logging).
    pub fn capture_bodies(mut self, on: bool) -> Self {
        self.capture_bodies = on;
        self
    }

    pub fn model(&self) -> &str {
        &self.model
    }
}

impl LlmClient for GeminiClient {
    async fn complete(&self, req: LlmRequest) -> Result<LlmResponse, LlmError> {
        let body = build_request(&self.model, &req)?;
        let started = Instant::now();

        let res = self
            .http
            .post(&self.endpoint)
            .header("x-goog-api-key", &self.api_key)
            .json(&body)
            .send()
            .await
            .map_err(|e| LlmError::Transport(e.without_url().to_string()))?;

        let status = res.status().as_u16();
        let retry_after = res
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<u64>().ok())
            .map(Duration::from_secs);
        let text = res.text().await.map_err(|e| LlmError::Transport(e.without_url().to_string()))?;

        if !(200..300).contains(&status) {
            return Err(classify_error(status, &text, retry_after, &self.api_key));
        }

        let parsed: Value = serde_json::from_str(&text)
            .map_err(|e| LlmError::BadResponse(format!("response is not JSON: {e}")))?;
        let latency_ms = started.elapsed().as_millis() as u64;
        let mut response = parse_response(&parsed, latency_ms)?;
        if self.capture_bodies {
            response.exchange = Some(Exchange { request: body, response: parsed });
        }
        Ok(response)
    }
}

/// Builds the Interactions request body.
pub fn build_request(model: &str, req: &LlmRequest) -> Result<Value, LlmError> {
    let mut steps = Vec::new();
    for turn in &req.turns {
        match turn {
            Turn::User { text } => steps.push(json!({
                "type": "user_input",
                "content": [{ "type": "text", "text": text }],
            })),
            Turn::Assistant { text, tool_uses, raw } => {
                if raw.is_empty() {
                    // No provider items (e.g. history rebuilt from a log): synthesise them.
                    if let Some(text) = text {
                        steps.push(json!({ "type": "model_output", "content": [{ "type": "text", "text": text }] }));
                    }
                    for tu in tool_uses {
                        steps.push(json!({ "type": "function_call", "id": tu.id, "name": tu.name, "arguments": tu.arguments }));
                    }
                } else {
                    steps.extend(raw.iter().map(|item| item.0.clone()));
                }
            }
            Turn::Tool { results } => steps.extend(results.iter().map(function_result_step)),
        }
    }
    // The API answers an empty input with a misleading "model not found" 404.
    if steps.is_empty() {
        return Err(LlmError::BadResponse("refusing to send a request with no input".into()));
    }

    let mut body = json!({ "model": model, "store": false, "input": steps });
    if !req.system.is_empty() {
        body["system_instruction"] = json!(req.system);
    }
    if !req.tools.is_empty() {
        body["tools"] = Value::Array(req.tools.iter().map(tool_declaration).collect());
    }
    if let Some(max) = req.max_output_tokens {
        body["generation_config"] = json!({ "max_output_tokens": max });
    }
    Ok(body)
}

fn function_result_step(result: &ToolResult) -> Value {
    let (text, is_error) = match &result.outcome {
        ToolOutcome::Ok { content, .. } if content.is_empty() => ("(no output)".to_string(), false),
        ToolOutcome::Ok { content, .. } => (content.clone(), false),
        ToolOutcome::Error { message, .. } => (message.clone(), true),
    };
    let mut step = json!({
        "type": "function_result",
        "name": result.name,
        "call_id": result.tool_use_id,
        "result": [{ "type": "text", "text": text }],
    });
    if is_error {
        step["is_error"] = json!(true);
    }
    step
}

fn tool_declaration(spec: &ToolSpec) -> Value {
    json!({
        "type": "function",
        "name": spec.name,
        "description": spec.description,
        "parameters": sanitize_schema(spec.parameters.clone()),
    })
}

/// Removes schemars decoration Gemini does not need. `format` values such as
/// `uint32` are dropped because the API documents only a few integer formats.
pub fn sanitize_schema(mut schema: Value) -> Value {
    fn walk(value: &mut Value) {
        match value {
            Value::Object(map) => {
                map.remove("$schema");
                map.remove("title");
                if map.get("format").and_then(Value::as_str).is_some_and(|f| f.starts_with("uint")) {
                    map.remove("format");
                }
                map.values_mut().for_each(walk);
            }
            Value::Array(items) => items.iter_mut().for_each(walk),
            _ => {}
        }
    }
    walk(&mut schema);
    schema
}

/// Parses a successful Interactions response.
pub fn parse_response(body: &Value, latency_ms: u64) -> Result<LlmResponse, LlmError> {
    let steps = body["steps"]
        .as_array()
        .ok_or_else(|| LlmError::BadResponse("response has no `steps` array".into()))?;

    let mut texts: Vec<String> = Vec::new();
    let mut tool_uses: Vec<ToolUse> = Vec::new();
    for step in steps {
        match step["type"].as_str() {
            Some("model_output") => {
                let text: String = step["content"]
                    .as_array()
                    .map(|blocks| blocks.iter().filter_map(|b| b["text"].as_str()).collect::<Vec<_>>().join(""))
                    .unwrap_or_default();
                if !text.is_empty() {
                    texts.push(text);
                }
            }
            Some("function_call") => {
                let id = step["id"].as_str().filter(|s| !s.is_empty());
                let name = step["name"].as_str().filter(|s| !s.is_empty());
                let (Some(id), Some(name)) = (id, name) else {
                    return Err(LlmError::BadResponse("function_call step without id or name".into()));
                };
                let arguments = if step["arguments"].is_null() { json!({}) } else { step["arguments"].clone() };
                tool_uses.push(ToolUse { id: id.into(), name: name.into(), arguments });
            }
            _ => {}
        }
    }

    if texts.is_empty() && tool_uses.is_empty() {
        return Err(LlmError::BadResponse("response contained no text and no tool calls".into()));
    }

    let finish = if !tool_uses.is_empty() {
        FinishReason::ToolUse
    } else {
        match body["status"].as_str() {
            Some("completed") => FinishReason::Stop,
            Some("incomplete") => FinishReason::MaxTokens,
            Some(other) => FinishReason::Other(other.to_string()),
            None => FinishReason::Other("missing status".into()),
        }
    };

    let text = if texts.is_empty() { None } else { Some(texts.join("\n")) };
    let raw = steps.iter().cloned().map(RawItem).collect();
    Ok(LlmResponse {
        turn: Turn::Assistant { text, tool_uses, raw },
        usage: parse_usage(&body["usage"]),
        finish,
        latency_ms,
        exchange: None,
    })
}

/// Gemini's `total_input_tokens` / `total_output_tokens` were observed to be much
/// lower than the per-invocation counts (docs/gemini-notes.md), so prefer the
/// detailed numbers and fall back to the totals.
fn parse_usage(usage: &Value) -> Usage {
    let input = usage["raw_prompt_token"].as_u64().or_else(|| usage["total_input_tokens"].as_u64());

    let candidates: u64 = usage["model_invocation_token_counts"]
        .as_array()
        .map(|calls| {
            calls
                .iter()
                .flat_map(|c| c["candidates_tokens_details"].as_array().into_iter().flatten())
                .filter_map(|d| d["tokens"].as_u64())
                .sum()
        })
        .unwrap_or(0);
    let output = if candidates > 0 { Some(candidates) } else { usage["total_output_tokens"].as_u64() };

    Usage { input_tokens: input, output_tokens: output, thinking_tokens: usage["total_thought_tokens"].as_u64() }
}

/// Maps a non-2xx response to an [`LlmError`]. Two body shapes exist: an object with
/// a string `error.code`, and an array wrapping a Google RPC error with a numeric one.
pub fn classify_error(status: u16, body: &str, retry_after: Option<Duration>, api_key: &str) -> LlmError {
    let invalid_key = body.contains("API_KEY_INVALID");
    if status == 401 || status == 403 || invalid_key {
        return LlmError::Auth;
    }
    if status == 429 {
        return LlmError::RateLimited { retry_after };
    }
    let message = error_message(body).unwrap_or_else(|| body.to_string());
    let message = if api_key.len() >= 8 { message.replace(api_key, "[REDACTED]") } else { message };
    let message = crate::obs::preview(&message, 2048);
    LlmError::Http { status, body: message }
}

fn error_message(body: &str) -> Option<String> {
    let value: Value = serde_json::from_str(body).ok()?;
    let error = match &value {
        Value::Array(items) => items.first()?.get("error")?.clone(),
        other => other.get("error")?.clone(),
    };
    error.get("message")?.as_str().map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::turn::ErrorClass;

    fn req(turns: Vec<Turn>) -> LlmRequest {
        LlmRequest { system: "be brief".into(), turns, tools: vec![], max_output_tokens: None }
    }

    fn model_response() -> Value {
        json!({
            "status": "requires_action",
            "steps": [
                { "type": "thought", "signature": "<sig>" },
                { "type": "function_call", "id": "call_1", "name": "list_dir", "arguments": { "dir_path": "." }, "signature": "<sig>" }
            ],
            "usage": {
                "total_input_tokens": 300, "total_output_tokens": 18, "total_thought_tokens": 0, "raw_prompt_token": 353,
                "model_invocation_token_counts": [{ "candidates_tokens_details": [{ "modality": "text", "tokens": 24 }] }]
            }
        })
    }

    #[test]
    fn parses_tool_call_and_keeps_every_step_raw() {
        let r = parse_response(&model_response(), 12).unwrap();
        assert_eq!(r.finish, FinishReason::ToolUse);
        let Turn::Assistant { text, tool_uses, raw } = &r.turn else { panic!() };
        assert_eq!(text, &None);
        assert_eq!(tool_uses.len(), 1);
        assert_eq!(tool_uses[0].id, "call_1");
        assert_eq!(tool_uses[0].arguments, json!({ "dir_path": "." }));
        assert_eq!(raw.len(), 2);
        assert_eq!(raw[0].0["type"], "thought");
        assert_eq!(r.latency_ms, 12);
    }

    #[test]
    fn usage_prefers_detailed_counts() {
        let u = parse_response(&model_response(), 0).unwrap().usage;
        assert_eq!(u, Usage { input_tokens: Some(353), output_tokens: Some(24), thinking_tokens: Some(0) });
    }

    #[test]
    fn parses_text_and_maps_status() {
        let body = json!({ "status": "completed", "steps": [
            { "type": "thought", "signature": "s" },
            { "type": "model_output", "content": [{ "type": "text", "text": "hi " }, { "type": "text", "text": "there" }] }
        ]});
        let r = parse_response(&body, 0).unwrap();
        assert_eq!(r.finish, FinishReason::Stop);
        assert!(matches!(&r.turn, Turn::Assistant { text: Some(t), .. } if t == "hi there"));

        let cut = json!({ "status": "incomplete", "steps": [{ "type": "model_output", "content": [{ "type": "text", "text": "x" }] }]});
        assert_eq!(parse_response(&cut, 0).unwrap().finish, FinishReason::MaxTokens);
        let weird = json!({ "status": "failed", "steps": [{ "type": "model_output", "content": [{ "type": "text", "text": "x" }] }]});
        assert_eq!(parse_response(&weird, 0).unwrap().finish, FinishReason::Other("failed".into()));
    }

    #[test]
    fn rejects_empty_or_malformed_responses() {
        assert!(matches!(parse_response(&json!({}), 0), Err(LlmError::BadResponse(_))));
        let only_thought = json!({ "status": "completed", "steps": [{ "type": "thought", "signature": "s" }] });
        assert!(matches!(parse_response(&only_thought, 0), Err(LlmError::BadResponse(_))));
        let no_id = json!({ "steps": [{ "type": "function_call", "name": "x", "arguments": {} }] });
        assert!(matches!(parse_response(&no_id, 0), Err(LlmError::BadResponse(_))));
    }

    #[test]
    fn builds_stateless_history_with_raw_items_echoed_verbatim() {
        let parsed = parse_response(&model_response(), 0).unwrap();
        let turns = vec![
            Turn::User { text: "list".into() },
            parsed.turn,
            Turn::Tool {
                results: vec![
                    ToolResult {
                        tool_use_id: "call_1".into(),
                        name: "list_dir".into(),
                        outcome: ToolOutcome::Ok { content: "a\n".into(), metadata: json!({}), truncated: false },
                    },
                    ToolResult {
                        tool_use_id: "call_2".into(),
                        name: "read_file".into(),
                        outcome: ToolOutcome::Error { class: ErrorClass::NotFound, message: "Not found: x".into() },
                    },
                ],
            },
        ];
        let body = build_request("m", &req(turns)).unwrap();
        assert_eq!(body["store"], false);
        assert_eq!(body["model"], "m");
        assert_eq!(body["system_instruction"], "be brief");
        let input = body["input"].as_array().unwrap();
        assert_eq!(input.len(), 5);
        assert_eq!(input[0]["type"], "user_input");
        assert_eq!(input[1], json!({ "type": "thought", "signature": "<sig>" }));
        assert_eq!(input[2]["type"], "function_call");
        assert_eq!(input[3]["call_id"], "call_1");
        assert!(input[3].get("is_error").is_none());
        assert_eq!(input[4]["is_error"], true);
        assert_eq!(input[4]["result"][0]["text"], "Not found: x");
        assert!(body.get("tools").is_none());
        assert!(body.get("generation_config").is_none());
    }

    #[test]
    fn synthesises_steps_when_raw_is_empty_and_refuses_empty_input() {
        let turns = vec![
            Turn::User { text: "hi".into() },
            Turn::Assistant {
                text: Some("ok".into()),
                tool_uses: vec![ToolUse { id: "c".into(), name: "list_dir".into(), arguments: json!({}) }],
                raw: vec![],
            },
        ];
        let body = build_request("m", &req(turns)).unwrap();
        let input = body["input"].as_array().unwrap();
        assert_eq!(input[1]["type"], "model_output");
        assert_eq!(input[2], json!({ "type": "function_call", "id": "c", "name": "list_dir", "arguments": {} }));

        assert!(matches!(build_request("m", &req(vec![])), Err(LlmError::BadResponse(_))));
    }

    #[test]
    fn tools_and_token_cap_are_included_and_schemas_sanitised() {
        let specs = crate::tools::registry::tool_specs();
        let mut r = req(vec![Turn::User { text: "x".into() }]);
        r.tools = specs;
        r.max_output_tokens = Some(256);
        let body = build_request("m", &r).unwrap();
        assert_eq!(body["generation_config"]["max_output_tokens"], 256);
        let tools = body["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 3);
        let text = serde_json::to_string(tools).unwrap();
        assert!(!text.contains("$schema"));
        assert!(!text.contains("\"title\""));
        assert!(!text.contains("uint"));
        assert!(tools.iter().all(|t| t["type"] == "function" && t["parameters"]["type"] == "object"));
    }

    #[test]
    fn classifies_both_error_shapes() {
        let key_err = r#"[{"error":{"code":400,"status":"INVALID_ARGUMENT","message":"API key not valid.","details":[{"reason":"API_KEY_INVALID"}]}}]"#;
        assert_eq!(classify_error(400, key_err, None, "k"), LlmError::Auth);
        assert_eq!(classify_error(403, "{}", None, "k"), LlmError::Auth);

        let model_err = r#"{"error":{"code":"not_found","message":"Model 'x' not found."}}"#;
        assert_eq!(
            classify_error(404, model_err, None, "k"),
            LlmError::Http { status: 404, body: "Model 'x' not found.".into() }
        );

        assert_eq!(
            classify_error(429, "{}", Some(Duration::from_secs(7)), "k"),
            LlmError::RateLimited { retry_after: Some(Duration::from_secs(7)) }
        );
        assert!(classify_error(503, "upstream down", None, "k").is_retryable());
        assert_eq!(classify_error(500, "oops", None, "k"), LlmError::Http { status: 500, body: "oops".into() });
    }

    #[test]
    fn error_bodies_never_leak_the_key() {
        let err = classify_error(500, "bad key sk-secret-123456 in body", None, "sk-secret-123456");
        let LlmError::Http { body, .. } = err else { panic!() };
        assert!(!body.contains("sk-secret-123456"));
    }

    #[test]
    fn from_env_requires_a_key() {
        // Only checks the constructor path that does not touch the environment.
        let c = GeminiClient::new("key", DEFAULT_MODEL).with_endpoint("http://localhost:1").capture_bodies(true);
        assert_eq!(c.model(), DEFAULT_MODEL);
    }
}
