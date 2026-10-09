# Gemini Interactions API: confirmed shapes

Probed 2026-10-10 with `crates/lightning/examples/gemini_probe.rs`, model `gemini-3.5-flash-lite` only, `store: false` throughout. These are observations from real calls, not documentation claims. Signatures below are shortened to `"<sig>"`.

Re-run: `cargo run --example gemini_probe -- <out_dir>` from `crates/lightning` (needs `GEMINI_API_KEY` in `crates/lightning/.env`). It writes one request/response pair per case; the key is only sent as a header and never written.

## Request

`POST https://generativelanguage.googleapis.com/v1beta/interactions`, header `x-goog-api-key: <raw key>`.

```json
{
  "model": "gemini-3.5-flash-lite",
  "store": false,
  "system_instruction": "plain string",
  "input": [ /* steps, see below */ ],
  "tools": [ { "type": "function", "name": "...", "description": "...", "parameters": { /* JSON Schema */ } } ],
  "generation_config": { "max_output_tokens": 16, "thinking_level": "high", "thinking_summaries": "auto" }
}
```

- `input` may be a plain string or an array of steps. Stateless multi-turn = resend the whole step array every request.
- `system_instruction` is a plain string and must be sent on every request (stateless).
- Step types sent by us:
  - `{"type":"user_input","content":[{"type":"text","text":"..."}]}`
  - model steps from earlier responses, **verbatim** (see below)
  - `{"type":"function_result","name":"<tool>","call_id":"<function_call.id>","result":[{"type":"text","text":"..."}],"is_error":true}` (`is_error` optional)
- Tool declarations take schemars output as is: the `$schema` and `title` keys were accepted (case 03a returned 200 and a correct call). Stripping them is optional; stripped and unstripped behaved the same. A nullable type (`["string","null"]`) was also accepted, but the model never called that tool, so tool use with nullable params is **unproven**.

## Response

```json
{
  "object": "interaction", "model": "gemini-3.5-flash-lite", "service_tier": "standard",
  "status": "completed | requires_action | incomplete",
  "created": "...", "updated": "...",
  "steps": [ ... ],
  "usage": { ... }
}
```

There is no top-level interaction `id` in `store:false` responses worth keeping; we do not use `previous_interaction_id`.

### Steps in responses

| `type` | Fields | Notes |
|---|---|---|
| `thought` | `signature`; with `thinking_summaries:"auto"` also `summary: [{type:"text", text}]` | Present on **every** response, even trivial ones. |
| `model_output` | `content: [{type:"text", text}]` | Final or intermediate text. |
| `function_call` | `id` (e.g. `call_2115697`), `name`, `arguments` (object), `signature` | `signature` is on the call itself too. |

### `status` values observed

- `completed`: model finished (text answer).
- `requires_action`: model returned `function_call` step(s); we must send results.
- `incomplete`: output cut by `max_output_tokens`; text up to the cap is still returned.
- Documented but not observed: `in_progress`, `failed`, `cancelled`, `queued`, `budget_exceeded` (deprecated).

Mapping for `FinishReason`: `completed` -> Stop, `requires_action` -> ToolUse, `incomplete` -> MaxTokens, anything else -> Other(status). Also treat any `function_call` step as ToolUse regardless of status.

### Usage

```json
"usage": {
  "total_input_tokens": 14, "total_output_tokens": 15, "total_thought_tokens": 0,
  "total_tokens": 29, "total_cached_tokens": 0, "total_tool_use_tokens": 0,
  "raw_prompt_token": 49,
  "model_invocation_token_counts": [{ "prompt_tokens_details": [{"modality":"text","tokens":49}],
                                       "candidates_tokens_details": [{"modality":"text","tokens":19}] }]
}
```

Caveat (observed, not explained): `total_input_tokens` (14) is far below `raw_prompt_token` / `prompt_tokens_details` (49) for the same call, and `total_output_tokens` (15) is below `candidates_tokens_details` (19). `total_thought_tokens` stayed 0 even though a `thought` step was returned. Treat the `total_*` fields as unreliable for cost; log both `total_*` and `raw_prompt_token` / the `candidates` sum, and do not build limits on either yet.

## Echoing history (stateless)

- Next request's `input` = all earlier `user_input` steps, all earlier model steps **exactly as received** (`thought`, `function_call`, `model_output`), and our `function_result` steps, in order.
- Probe 06 dropped the `thought` steps but kept the `function_call` (with its `signature`) and the API still returned 200 and a sensible answer. So thoughts are not strictly required on this model, but the spec rule stands: echo every model step verbatim via `raw` and never edit them; dropping them is untested for longer chains and for thinking-heavy models.
- `function_call.id` -> `function_result.call_id`, and `name` must match.
- Probe 09e sent a `function_result` with a `call_id` that matches nothing: **no error** (200), the model just called the tool again. So the API does not enforce pairing; the harness must (every `function_call` gets exactly one result).
- Probe 05 sent `is_error: true`: accepted; the model retried with different arguments.

## Errors

Two different shapes were seen, so the adapter must handle both:

1. Interactions-style (bad model): HTTP 404, body `{"error":{"code":"not_found","message":"Model '...' not found. ..."}}`. `code` is a **string**.
2. Google RPC style (bad key): HTTP **400** (not 401/403), body is a **JSON array** `[{"error":{"code":400,"status":"INVALID_ARGUMENT","message":"API key not valid...","details":[{"reason":"API_KEY_INVALID", ...}]}}]`. `code` is a **number**.

Consequences for `LlmError`:
- Classify auth by `details[].reason == "API_KEY_INVALID"` (and HTTP 401/403), not by status alone.
- Parse the error body as either an object or an array of objects; on parse failure keep the raw text.
- Misleading error: an **empty `input` array** returned 404 `Model 'gemini-3.5-flash-lite' not found`, although the model exists. A 404 "model not found" therefore does not prove the model name is wrong; the adapter should never send an empty `input`, and the message should be logged with the request step count.
- 429 was hit for real during the T9 end-to-end run (a normal CLI session on `gemini-3.5-flash-lite`): the response carried a retry delay of 16 s, which `classify_error` turned into `RateLimited { retry_after: Some(16s) }`; the session waited, retried once and completed. 5xx bodies are still unobserved.

## Other facts

- Tool-call example from the live model, asked to list the workspace root: `{"name":"list_dir","arguments":{"dir_path":"."},"id":"call_...","type":"function_call"}`. After an error result it retried with `dir_path: ""`, which our `Workspace` must handle (joins to the root).
- Round-trip latency for these small calls was about 1 s.
- Each response carries `service_tier: "standard"`.

## Still unverified

- Parallel function calls (several `function_call` steps in one response).
- 5xx / timeout bodies. (429 handling was seen working; see Errors.)
- Behaviour with a tool schema that has nullable params when the model does call it.
- Whether `thought` steps become mandatory with `thinking_level: "high"` plus tool calls over several steps.
- Context-size limits and the error returned when exceeded.
