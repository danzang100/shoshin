# Measurement plan: repo and device modes E1–E6 only

> **Superseded in part (2026-10-09):** the LiteLLM-only decision was reversed. The harness will call providers directly, in Rust, with no proxy and no Python: local OpenAI-compatible servers, Gemini, Anthropic (native Messages API) and OpenAI. Sections that mention LiteLLM, `x-litellm-*` headers or a single wire format are out of date. See the conversation notes for the revised LLM-client plan.


Date: 2026-10-09. Status: draft, design only. Narrows [measurement-plan.md](measurement-plan.md) to the six modes in the "Repo and device" group of [failure-modes.md](failure-modes.md). Modes borrowed from other groups (C8, C2, T11, T3/T4) are **out of scope here**.
Labels: **[M]** measured from repo/spec, **[S]** stated by a cited source, **[E]** my estimate.

## 1. The six modes and what can be measured when

| ID | Mode | Needs | Measurable today? |
|---|---|---|---|
| E5 | Windows breakage | `Workspace` only | **Yes**: as tests, now |
| E4 | Model throughput / wait time | LLM client (streaming for TTFT) + LiteLLM headers | After the client exists |
| E1 | Guessing the build/test command | `run_shell` | After `run_shell` exists |
| E3 | Slow tool / timeout | `run_shell` with timeouts, tool timing | After `run_shell` exists |
| E6 | Flaky / pre-existing test failures | `run_shell` + test-output parsing | After `run_shell` exists |
| E2 | Rediscovering the repo each session | ≥ ~10 sessions in the same repo | After sessions accumulate |

Reality check [M]: `run_shell`, the orchestrator, the session store and the working LLM client do not exist yet. Only E5 can be measured today. The rest are specified here so the fields are designed in.

## 2. Fields required (only what E1–E6 need)

| Record | Fields |
|---|---|
| Shell call | `argv0`, `subcommand`, `intent` (from a fixed table: build / test / check / fmt / other), `spawn_result` (ok / spawn_not_found / spawn_other), `exit_code`, `duration_ms`, `timed_out`, `output_bytes`, `writes_since_last_same_command` (bool) |
| Tool call (any) | `tool`, `duration_ms` |
| LLM call | `alias`, `answered_model`, `local_or_cloud`, `prompt_tokens`, `completion_tokens`, `total_ms`, `gateway_ms`, `gateway_overhead_ms`, `ttft_ms` (streaming only), `retries`, `fallbacks`, `finish_reason` |
| Session | `repo_key` (path + manifest hash), `git_head`, `tree_dirty_hash`, `os`, ordered list of normalized `(tool, path-or-arg-shape)` for the opening phase |
| Test run | `git_head`, `tree_dirty_hash`, `command_hash`, `outcome` (pass / fail), failing test names if parseable |

Storage is SQLite. Metrics exported to Mimir carry only `backend`, `model`, `tool`, `intent`, never path, repo or session id.

## 3. Per-mode measurement

### E5: Windows breakage (measure now)

- **Method: tests first, telemetry second.** A fixture suite against `Workspace::resolve` and the file tools, run on Windows. Cases: `fs::canonicalize` output form (`\\?\` prefix) vs. the form shown to the model; drive-letter case differences; mixed `/` and `\`; `..` traversal; symlink/junction escaping the root; CRLF round trip through read and write; 8.3 short names; reserved device names (`CON`, `NUL`); paths over 260 characters. Whether `starts_with` containment behaves correctly for case differences on Windows is **a test to run, not an assumption** [M: containment uses `canonicalize` + `starts_with`, `tools/workspace.rs`].
- **Production counter:** `path_form_errors` = `ToolError::Io`/`NotFound` where the path contains `\\?\` or mixed separators.
- **Target:** all fixtures green; counter 0.
- **Sample size:** none; deterministic.
- **Kill criterion:** none. This is a correctness test suite, not an optimization.

### E4: Model throughput and wait time (measure when the client exists)

Backends are reached only through LiteLLM's OpenAI-compatible API (decided 2026-10-09). Native backend timings (Ollama durations, llama.cpp `timings`) are therefore **not assumed**; vLLM reports timing as server-side Prometheus metrics rather than in responses [S: vLLM docs]. E4 uses only what the harness observes itself plus LiteLLM's headers.

- **Per-call inputs:** wall-clock total; `x-litellm-response-duration-ms` and `x-litellm-overhead-duration-ms` (so provider time ≈ response duration − overhead); `usage.prompt_tokens` / `completion_tokens`; **time-to-first-token if streaming** (client-side `Instant`); retries and fallbacks; the model that actually answered.
- **Metrics (keyed by answered model, tagged `local` or `cloud` from the registry):**
  - `ttft_ms` against `prompt_tokens` (a scatter or per-1k-token bucket): the observable stand-in for prefill cost. It includes queueing and any cache effect, which cannot be separated without backend metrics.
  - `decode_tok_s = completion_tokens / (total − ttft)`.
  - `wait_share` = Σ TTFT ÷ session wall time. This replaces the earlier `prefill_share`.
  - If not streaming: fit `total ≈ a·prompt + b·completion + c` over ≥ 50 calls (rougher; cache hits bias `a`).
- **Exclusions:** drop calls with retries or fallbacks > 0 from the speed series (keep them in a separate count); require ≥ 500 prompt tokens for the prefill proxy.
- **Sample size:** ≥ ~50 qualifying calls per answered model before the medians are trusted [E].
- **Action threshold [E]:** adapt (stricter pruning, smaller per-turn budget) only if `wait_share` > 40% **and** the model is tagged `local` **and** the user confirms. Cloud numbers describe the provider, not the machine, so they never trigger machine-level adaptation.
- **Kill criterion:** `wait_share` stays under ~15% [E].
- **Optional enrichment, no harness code:** scrape vLLM `/metrics` and LiteLLM's Prometheus endpoint through the collector for queue time, prefill time, KV-cache usage and prefix-cache hit counters (unverified against the otel-lgtm image).
- **Caveats:** the pass-through of cache-token details in `usage` is undocumented in what I fetched, so no mode depends on it. Machine load adds noise: use medians and keep the p10/p90 spread.

### E1: Guessing the build/test command

- **Classification of each shell call:**
  - `spawn_not_found`: the executable doesn't exist. With argv spawning this surfaces as a spawn error, not an exit code, so it's unambiguous.
  - `ran_ok` / `ran_nonzero`: the program ran. A failing test run is *legitimate*, not an E1 failure.
  - `usage_error`: recognized only through a **fixed ecosystem table** (e.g. cargo's unknown-subcommand message). Anything not in the table is `ambiguous` and excluded from the metric, not guessed.
- **Metrics (per session, per intent):** `attempts_before_first_ok`, `spawn_not_found` count, and `profile_hit` (the first command for that intent equals the confirmed profile command, once a profile exists).
- **Trigger for a proposal:** one observed failure-then-success pair is enough evidence, because the user confirms before anything is stored.
- **Target:** in a profiled repo, 1 attempt per intent and `profile_hit` ≥ 90% [E].
- **Sample size:** descriptive only. Judge per repo before/after the profile exists; with few sessions per repo, report raw counts, not rates.
- **Kill criterion:** if first attempts already succeed in ≥ 95% of sessions without a profile, E1 is not a real problem for your repos and R2 (command discovery) shrinks to a stored hint.
- **Dependency:** `run_shell` and the permission allowlist.

### E3: Slow tool / timeout

- **Metrics:** `duration_ms` percentiles per (`tool`, `argv0`+`intent`); `timeout_rate`; `wasted_wall_s` = total time in calls that timed out.
- **Trigger:** act only when the **same command** times out in ≥ 2 sessions. Until then keep generous static timeouts.
- **Proposed action:** raise that command's timeout to ~3× observed p95, **with user confirmation**, capped by a global maximum.
- **Sample size:** ≥ 10 samples of a command for a stable p95 [E].
- **Kill criterion:** if no timeouts occur in the first ~30 sessions, leave timeouts static.
- **Caveats:** durations vary with machine load, cold disk cache and incremental build state. A first `cargo build` after a clean can be 10× a warm one, so track `intent` and whether the build was warm (e.g. whether `target/` existed) before comparing.

### E6: Flaky or pre-existing test failures

- **Free baseline, no extra runs:** key every test run by (`git_head`, `tree_dirty_hash`, `command_hash`).
  - *Pre-existing failure evidence:* the first test run of a session on a clean tree (before any write) that fails.
  - *Flaky evidence:* two runs with the same key and different outcomes.
- **Metrics:** `baseline_fail_rate` (share of clean-tree first runs that fail), `flaky_rate` (share of keys with mixed outcomes).
- **Parsing:** pass/fail from the exit code; failing test names only via per-ecosystem parsers (e.g. libtest output for Rust). If no parser is available, record outcome only.
- **Target:** none. This is a gate, not an optimization.
- **Kill criterion:** if `baseline_fail_rate` and `flaky_rate` are both under ~5% [E] across ~20 test runs, drop E6.
- **Dependency:** `run_shell`, plus a test-run record. An *extra* baseline run per commit is not part of the plan unless the free evidence shows the problem exists.

### E2: Rediscovering the repo each session

- **Why not a sequence hash alone:** tasks differ, so exact opening sequences rarely repeat. Use the more robust form:
  - *Opening phase* = tool calls before the first non-read-only call, capped at 8.
  - **Core set** = files and directories read in ≥ 50% of a repo's sessions.
- **Metrics:** `core_set_size`, `core_set_tokens` (tokens those reads cost per session), `opening_tokens` (mean per session).
- **Decision rule:** build a cached repo map only if `core_set_tokens` per session exceeds the map's per-session cost. Rough map cost [E]: a ~2k-token map is ~$0.008/session on Sonnet 5.5 when cached (one write at $2.50/MTok, ~15 reads at $0.10/MTok), though it also adds standing context and can go stale. For a local model the comparison is prefill seconds instead of dollars.
- **Sample size:** ≥ ~10 sessions in the same repo for a core set to mean anything [E].
- **Kill criterion:** if the core set is empty or under ~1k tokens per session, drop E2.
- **Dependency:** accumulated sessions; nothing to build before that except the opening-phase logging.

## 4. Validating the measurements themselves (not experiments)

1. **Fixture tests for every classifier**, including negative cases (a failing test run is not an E1 failure; a legitimate command retry after a write is not a repeat).
2. **Shadow mode first:** classifiers log findings and change nothing.
3. **Spot-check before any action:** hand-review ~20 findings per mode; require ≥ 90% to be real before a finding may trigger a user prompt.
4. **One change at a time**, with the "before" taken from shadow-mode data. No other attribution is possible at this volume.

## 5. Order

| Step | Mode | Condition |
|---|---|---|
| 1 | E5 tests | now |
| 2 | Log fields for E4 | when the LLM client streams and reads the LiteLLM headers |
| 3 | Shell classification (E1), timing (E3), test-run keying (E6) | when `run_shell` exists |
| 4 | E4 baseline | after ~50 qualifying calls |
| 5 | E1, E3, E6 decisions against their kill criteria | after ~30 sessions |
| 6 | E2 core-set analysis | after ~10 sessions in one repo |

## 6. Open questions

1. **E6 scope:** are you willing to run an extra baseline test pass per commit if free evidence shows flakiness, or should E6 stay opportunistic only?
2. ~~Local runtime~~ **Decided:** everything via LiteLLM, OpenAI-compatible; local served by vLLM or similar.
3. **Timeout authority:** may the harness raise a timeout cap by itself within a hard ceiling, or must you confirm every increase?
