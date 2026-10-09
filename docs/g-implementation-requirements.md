# What the code needs to start all repo/device items (E1–E6) at once

> **Superseded in part (2026-10-09):** the LiteLLM-only decision was reversed. The harness will call providers directly, in Rust, with no proxy and no Python: local OpenAI-compatible servers, Gemini, Anthropic (native Messages API) and OpenAI. Sections that mention LiteLLM, `x-litellm-*` headers or a single wire format are out of date. See the conversation notes for the revised LLM-client plan.


Date: 2026-10-09. Status: requirements only, nothing implemented.
Scope: E1 command guessing, E2 rediscovery, E3 slow tools/timeouts, E4 throughput, E5 Windows breakage, E6 flaky/pre-existing tests. See [measurement-plan-repo-device.md](measurement-plan-repo-device.md).
Backends: local **and** cloud, all reached through LiteLLM (OpenAI-compatible only). Labels: **[M]** measured from the repo, **[S]** cited source, **[E]** my estimate (Rust-learner hours, low / central / high).

## 1. Starting point [M]

| Area | State today |
|---|---|
| Build | `cargo check -p lightning` fails with 12 errors, all in `llm/mod.rs` and `llm/openai.rs` (`from_env` fills fields with `()`, `chat` uses undefined variables, `into_response` mismatches types). |
| Tools | `read_file`, `list_dir`, registry, `Workspace`. Registry has an empty test (`registry.rs:90`). No other tests. |
| Tool results | `ToolOutput {content, metadata: serde_json::Value}`; metadata is `line_count` / `item_count` only. No timing, bytes, truncation flag. |
| Errors | `ToolError` has `NotFound`, `PathOutsideProject`, `InvalidArguments`, `Io`, `Other`. `ExecutionFailed` and `Timeout` are commented out (`error.rs:9-10`). |
| LLM | `ChatMessage {role: String, content: String}` with no `tool_call_id`; `LLMResponse` has no usage or timings; one tool call per response; no timing capture. |
| Missing entirely | `run_shell`, permission gate, orchestrator loop, session store (no `rusqlite`/`sqlx`), profile store, any `tracing::` call. |
| Dependencies | `tokio` has only `macros` and `rt-multi-thread` (no `process`, `time`, `sync`, `fs`); `reqwest` has `json` only; no hashing, TOML, uuid, time or path-dirs crates. |
| Entry point | `main.rs` is a one-shot menu on a hardcoded `test-folder` workspace. No repo selection. |

## 2. Which items depend on what

| Need | E1 | E2 | E3 | E4 | E5 | E6 |
|---|:-:|:-:|:-:|:-:|:-:|:-:|
| Compiling LLM client reporting usage + gateway headers (+ streaming for TTFT) | | | | ● | | |
| `run_shell` (argv spawn, timeout, output cap) | ● | | ● | | | ● |
| Orchestrator loop producing sessions | ● | ● | ● | ● | | ● |
| SQLite call/tool/test-run records | ● | ● | ● | ● | ○ | ● |
| Repo identity (root, manifest hash, git head, dirty hash) | ● | ● | ○ | | | ● |
| Harness-level profile store (outside repo) + confirm prompt | ● | ○ | ● | ○ | | |
| Detectors (pure functions + fixtures) | ● | ● | ● | ● | ● | ● |
| Windows path fixtures | | | | | ● | |

● required, ○ useful. **E5 needs none of the others.** **No tokenizer is needed** for any E-item: they rely on backend-reported counts and wall-clock time.

## 3. Work packages, in dependency order

Hours are [E], low / central / high.

### WP0: Make the crate compile and testable (3 / 6 / 12 h)
- Fix `llm/mod.rs` and `llm/openai.rs` enough to compile. A stub that returns `Unimplemented` is fine; WP2 replaces it.
- Fill the registry test; add `cargo test` as a habit. Every later package ships with tests.
- Reason first: nothing else can be built or tested until `cargo check` is green.

### WP1: Record types and error taxonomy (6 / 12 / 24 h)
New module (name placeholder: `record`), plain structs with `serde` derives:
- `LlmCallRecord`: `alias`, `answered_model`, `local_or_cloud`, timestamps, `prompt_tokens`, `completion_tokens`, `cached_tokens: Option` and `cache_write_tokens: Option` (reserved, tolerant parse), `total_ms`, `gateway_ms`, `gateway_overhead_ms`, `ttft_ms: Option` (streaming only), `http_status`, `retries`, `fallbacks`, `finish_reason`, `error_class`.
- `ToolCallRecord`: `tool`, `args_hash`, `duration_ms`, `result_bytes`, `error_class`, `path_class`.
- `ShellRunRecord`: `argv0`, `subcommand`, `intent`, `spawn_result`, `exit_code`, `duration_ms`, `timed_out`, `output_bytes`, `writes_since_last_same_command`.
- `TestRunRecord`: `git_head`, `tree_dirty_hash`, `command_hash`, `outcome`, failing names if parsed.
- `SessionRecord`: `repo_key`, `git_head`, `os`, `backend`, `model`, `outcome`, opening-phase list for E2.

Error changes in `tools/error.rs`: uncomment and implement `ExecutionFailed` and `Timeout`; add `SpawnFailed {kind}`; **split `InvalidArguments`** into bad-JSON, schema error and unknown tool. Add `fn class(&self) -> ErrorClass` so records never store free-text messages.
Replace ad-hoc `metadata` JSON with a typed `ToolMeta {bytes, duration, truncated}` plus the existing JSON for tool-specific extras.

### WP2: LLM client, one wire format via LiteLLM (16 / 30 / 55 h)
**Decision (2026-10-09):** every backend, cloud and local, is reached through the LiteLLM proxy using the OpenAI-compatible `/chat/completions` format. Local models behind LiteLLM are vLLM or other OpenAI-compatible servers, not necessarily Ollama. So there is **one `Backend` implementation**, no Ollama-native adapter, and no Anthropic-native adapter. The existing `llm/openai.rs` direction was already right.

What this does to measurement (the important part):

| Signal | What the harness can rely on through LiteLLM | Source |
|---|---|---|
| Token counts | `usage.prompt_tokens`, `completion_tokens`, `total_tokens` always. With `stream_options.include_usage` the final streamed chunk carries them | LiteLLM usage docs |
| Cache / reasoning token details | Not documented on the usage page I fetched. **Parse tolerantly**: every extra field `Option` with `#[serde(default)]`, unknown fields ignored. Do not build a mode that needs them | unverified |
| Gateway timing | Response headers `x-litellm-response-duration-ms` (total time inside the proxy) and `x-litellm-overhead-duration-ms` (LiteLLM's own overhead), so provider time ≈ response duration − overhead | LiteLLM response-headers docs |
| Retries / fallbacks | `x-litellm-attempted-retries`, `x-litellm-max-retries`, `x-litellm-attempted-fallbacks` | same |
| Which model actually answered | `x-litellm-model-id`, `x-litellm-model-group`, `x-litellm-model-api-base`, plus the `model` field in the body. A router or fallback can serve a different model than the one requested, so **key every E4 sample by the model that answered** | same |
| Cost (cloud) | `x-litellm-response-cost` and input/output/cache-read/cache-write splits | same |
| Backend-native timings (Ollama durations, llama.cpp `timings`) | **Not available per request.** Whether LiteLLM passes provider-specific extras through is undocumented in what I fetched, so assume no | unverified |
| vLLM per-request timing | The vLLM docs describe timing as Prometheus metrics at `/metrics` (time to first token, prefill time, queue time, KV-cache usage, prefix-cache hits), not in API responses | vLLM metrics docs |
| Time to first token | Only measurable by the harness if the request **streams** (client-side `Instant`). LiteLLM's own TTFT metric is also streaming-only | LiteLLM Prometheus docs |

Required in the client:
- Fix `ChatMessage` (role enum, `tool_call_id`, tool-result messages). `LLMResponse` carries `calls: Vec<ToolCall>` plus a `BackendStats` struct whose fields are all `Option`.
- Wrap every request in `Instant`; read the `x-litellm-*` headers listed above into `BackendStats`; store `answered_model`.
- Record `http_status`, `retry_count`, `fallbacks`, `finish_reason`, so retries and overload waits are never mistaken for slow hardware.
- A harness-level **model registry** in config: each LiteLLM alias is tagged `local` or `cloud` by you. Do not infer it from the `api_base` host.
- Secrets: the LiteLLM key from env or a harness-level config file only; never stored in records or logs. Redact `Authorization` in any request logging.

**Streaming (reverses my earlier "non-streaming first" advice).** With native Ollama/llama.cpp timings off the table, time-to-first-token is the only clean per-request latency signal, and it needs streaming. Cost: parse server-sent events (`reqwest` byte stream plus a small line parser or an SSE crate; check whether `reqwest` 0.13 needs a feature flag for streaming) and **assemble tool-call arguments from streamed fragments** (they arrive in pieces, indexed by call). Streaming also gives you live output in the CLI/GUI, which a chat-style agent usually wants anyway. Fallback if you prefer non-streaming: fit `total_latency ≈ a·prompt_tokens + b·completion_tokens + c` by least squares over ≥ 50 calls to estimate prefill and decode rates. It is rougher, and prompt-cache hits confound `a`.

### WP3: `run_shell` and a minimal permission gate (12 / 24 / 45 h)
- `tokio::process::Command` with argv (no shell string), working directory = workspace root, `kill_on_drop(true)`, `tokio::time::timeout`, stdout/stderr capture with a hard byte cap and a truncation flag. Enable tokio features `process`, `time`, `io-util`.
- Return a typed result separating `spawn_failed`, `exited(code)`, `timed_out`.
- **Windows pitfall to test before trusting `spawn_not_found`:** Rust implements its own executable search on Windows, and users report it failing to find programs the shell finds (e.g. `py`) [S: Rust forum, rust-lang tests `windows-spawn`]. Tools shipped as `.cmd`/`.bat` shims (such as `npm`) may not resolve by bare name [E: needs a fixture test]. Resolve the executable yourself first (a `which`-style lookup honoring `PATHEXT`), and only count `spawn_not_found` if that lookup also fails. Otherwise E1 will report false "wrong command" events on Windows.
- Permission gate: `enum Decision {Allow, Deny{reason}, AskUser}`; allowlist keyed by (`argv0`, `subcommand`), loaded from the profile store (WP7). Log every decision. It sits between registry lookup and execution, as in the spec.

### WP4: Windows path layer and E5 fixtures (6 / 12 / 24 h) — can start right after WP0
- Evaluate `dunce::canonicalize`, which returns ordinary `C:\...` paths where possible and leaves `\\?\` only when it can't be simplified [S: dunce docs]. Use it for paths shown to the model and written to records, while keeping containment checks on one consistent form.
- Fixture tests listed in the measurement plan (§3, E5): prefix forms, drive-letter case, mixed separators, `..`, junctions, CRLF round trip, 8.3 names, reserved names, long paths.
- Whether `Path::starts_with` containment is case-insensitive on Windows is a **test to write, not an assumption**.

### WP5: Session store (SQLite) (10 / 20 / 40 h)
- `rusqlite` with the `bundled` feature (no system SQLite on Windows). Tables: `sessions`, `llm_calls`, `tool_calls`, `shell_runs`, `test_runs`, `findings`, `schema_version`, plus the spec's `events` table if you keep it.
- One **writer task** owning the connection, fed by a `tokio::sync::mpsc` channel, so the loop never blocks on disk. Add tokio feature `sync`.
- Choice to confirm: `rusqlite` + writer task (recommended, small fixed schema) vs `sqlx` (compile-time checked queries, heavier setup). The spec allows either.
- Crates: `uuid`, `time` or `chrono`.

### WP6: Orchestrator loop, minimal (14 / 28 / 55 h)
- `Turn` enum, `AgentState`, `Outcome {Completed, MaxIterationsReached, Failed}` per the spec; max-iteration cap; naive context assembly (system prompt + task + last N turns).
- Wraps every LLM call and tool call with the record types from WP1 and sends them to the writer (WP5). Records are produced **at the call site**, not reconstructed afterward.
- Needs a working client (WP2), tools incl. `run_shell` (WP3), gate.
- `main.rs`: select the workspace by CLI argument or current directory instead of the hardcoded `test-folder`, and pick backend/model from config.

### WP7: Repo identity and harness-level profile store (8 / 16 / 30 h)
- `repo_key` = canonical repo root + hash of manifest files (`Cargo.toml`, `Cargo.lock`, `package.json`, `Makefile`, `pyproject.toml`). Crates: `sha2` or `blake3`.
- `git_head` and `tree_dirty_hash` by running `git` through the same argv runner (`git rev-parse HEAD`, `git status --porcelain`, hash of the output). Avoids a heavy `git2` dependency.
- Store under the OS data/config directory (`directories` crate), as TOML via `toml` + `serde`, **outside any repo**, keyed by `repo_key` and by backend + model. Each fact records evidence and date. A repo-resident file is read only as a suggestion.
- Confirm prompt using `inquire` (already a dependency): one proposal per session, decline remembered.

### WP8: Detectors (E1–E6) and shadow mode (16 / 30 / 55 h)
Pure functions `fn detect(&Records) -> Vec<Finding>`, one module per item, fixture-tested with negative cases:
- **E1:** shell classification + intent table + attempts per intent (fixed table of known ecosystems).
- **E2:** opening-phase capture and core-set computation per repo.
- **E3:** duration percentiles per (tool, intent); timeout counts; trigger on the same command timing out in ≥ 2 sessions.
- **E4:** rolling medians per answered model (tagged local or cloud): `ttft_ms` vs. prompt size, `decode_tok_s`, `wait_share`; exclude calls with retries or fallbacks; qualifying-call filter (≥ 500 prompt tokens).
- **E5:** production path-form counter.
- **E6:** test-run keying; flaky/pre-existing evidence from repeated keys; libtest parser for Rust, outcome-only elsewhere.
Findings go to a `findings` table. A global `shadow = true` flag means no detector takes an action.

### WP9 (parallel, not blocking): telemetry export (12 / 24 / 45 h)
`tracing` spans and `tracing-opentelemetry` + `opentelemetry-otlp`, metrics with labels limited to `backend`, `model`, `tool`, `intent`. SQLite is the source of truth, so the E-items can run without this. Do it in parallel because it is also your LGTM learning goal.

## 4. Critical path

```
WP0 ─┬─ WP4 (E5 fixtures)                     ← start immediately, independent
     └─ WP1 ─┬─ WP2 (client) ─┐
             ├─ WP3 (shell) ──┼─ WP6 (loop) ─ WP8 (detectors, shadow) 
             └─ WP5 (store) ──┘        ▲
                    WP7 (repo identity, profiles) ┘
WP9 (OTel) runs alongside after WP1
```

Sequencing note: WP2, WP3 and WP5 can be built in parallel once WP1's types exist. WP6 is the integration point, and nothing produces sessions before it.

Rough totals [E]: central ≈ 172 h (low ≈ 92, high ≈ 335), of which WP9 is ≈ 24 h. These are mostly work the spec already requires; the part that exists *only* for the E-items is WP4, the record types in WP1, WP7's profile store and WP8, roughly 40–60 h central.

## 5. New dependencies and feature flags (summary)

| Crate or feature | For |
|---|---|
| `tokio` features `process`, `time`, `sync`, `io-util` (`fs` if needed) | WP3, WP5, WP6 |
| `reqwest` (stream feature if 0.13 requires one; check), small SSE parser or an SSE crate | WP2 streaming |
| `rusqlite` (`bundled`) | WP5 |
| `uuid`, `time` or `chrono` | WP1, WP5 |
| `sha2` or `blake3` | WP7 |
| `toml`, `directories` | WP7 |
| `dunce`, a `which`-style lookup | WP3, WP4 |
| `tracing-opentelemetry`, `opentelemetry`, `opentelemetry_sdk`, `opentelemetry-otlp` | WP9 |

## 6. Deliberately not needed for E1–E6

A tokenizer; parallel tool-call execution; Ollama-native and Anthropic-native adapters; the context manager beyond naive truncation; the GUI; RAG; MCP; the user-intent group. Cache fields are cheap to reserve in the records now, so reserve them, but nothing here depends on them.

## 7. Decisions

Resolved (2026-10-09):
- **Backends:** everything through LiteLLM, OpenAI-compatible only. Local models are served by vLLM or other OpenAI-compatible servers, not necessarily Ollama.

Still open:
1. **Streaming:** recommended, for clean time-to-first-token and live output, at the cost of SSE parsing and tool-call fragment assembly (≈ +10–15 h [E]). The alternative is non-streaming plus the regression fallback in WP2.
2. **`rusqlite` + writer task or `sqlx`?** I recommend `rusqlite`.
3. **Workspace selection:** CLI argument or current directory?
4. **Local/cloud tagging:** OK to maintain a small model registry in harness config (alias → `local` or `cloud`)? The alternative of guessing from the host is fragile behind a gateway.
5. **Device metrics outside the harness (optional, later):** vLLM and LiteLLM both expose Prometheus `/metrics`. The otel-lgtm collector could scrape them into Mimir with no harness code, giving server-side queue, prefill and KV-cache gauges. Worth it? (Scraping config is unverified against the otel-lgtm image.)
