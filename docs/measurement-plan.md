# Priority group and measurement plan

Date: 2026-10-09. Status: draft, design only. Nothing implemented, no experiments run.
Builds on [failure-modes.md](failure-modes.md) and [repo-device-profiles.md](repo-device-profiles.md).
Labels: **[M]** measured from repo/spec, **[S]** stated by a cited source, **[E]** my estimate.
Decision recorded: confirmed facts are stored at the **harness level, outside any repo** (see §6).
Scope update: the active scope is narrowed to repo/device modes E1–E6, see [measurement-plan-repo-device.md](measurement-plan-repo-device.md). This file remains as the wider reference.

---

## 1. Which group first

**Group G (repo and device), narrowed to a first slice.** The slice borrows a few modes whose IDs sit in groups B and C, because they share the same measurements.

| Slice | Modes | Why now |
|---|---|---|
| **1a: invariants** (must be zero, need no history) | C8 context mismatch and silent truncation; C2 cache-busting prefix; T11 ignored-path reads; T4/T3 unbounded output and whole-file reads; E5 Windows breakage | Deterministic, work from call #1, prevent *silent* failures, and the checks are assertions rather than statistics. |
| **1b: distributions** (need `run_shell`) | E1 build/test command guessing; T10 repeated build/test | Highest-frequency repo failure. Measured as attempts per intent. |
| **1c: measure only, don't act yet** | C3 cold-cache gaps; E4 throughput; P1/P3/P5 per-model protocol errors; E2 repeated exploration | Need real gap, speed and session distributions before any adaptation is justified. |

Reasons for this ordering:
1. **It's the cheapest group to verify.** Most detectors are pure functions over one call record, so they can be unit-tested with fixtures before any session exists.
2. **The fields it needs are the fields every other group needs** (per-call usage and timings, prefix hashes, tool result sizes and error classes). Building them first de-risks everything later, including the deferred user-intent group.
3. **It prevents wrong answers, not just spend.** Silent truncation (C8) is the clearest case: the model answers from a cut prompt and nothing errors.
4. **The dollar value of pure token trimming is modest under caching** (earlier report), so the first slice favors correctness and latency over trimming.

**Next slice (2), once 1a/1b are in:** the cheap inline waste detectors from group B (T1 re-read, T2 repeated search, T5 retry-unchanged, T6 error streaks, T8 verify-by-rereading) and L1/L2 loop guards. They reuse the same call record, so they are nearly free to add. I'd also move T7 (serial calls) up early because it is a client design flaw rather than a behavior [M]: the client keeps only the first tool call.

**Not first, and why:** group A (deferred by you); group B as a whole (real but small in dollars, and mostly addressed by tool design plus the same fields); T12 and P7 (no cheap detector).

---

## 2. How measurement is structured

Four layers, each cheap:

| Layer | What | Cost |
|---|---|---|
| **Call record** | One row per LLM call: backend, model, timings, usage, prefix hashes, estimated vs. reported prompt tokens, gap since previous call | A few hundred bytes |
| **Tool record** | One row per tool call: tool, normalized args hash, result bytes and estimated tokens, duration, error class, truncated flag, path class | A few hundred bytes |
| **Session fingerprint** | ~20 integers/enums written at session end (counts and maxima of the findings below) | One row |
| **Metrics export** | Low-cardinality counters and histograms to Mimir, labeled only by `backend`, `model`, `tool`, `finding`. Never by path, repo or session id | Normal OTel cost |

Detectors are pure functions `fn detect(&CallRecord, &SessionState) -> Vec<Finding>` that run inline. They read no transcripts and call no LLM. Drill-down uses the stored `trace_id` into Tempo.

Two kinds of measure, handled differently:

- **Invariants** must be 0 (e.g. an unexplained prefix break). Any occurrence is a bug. No baseline or sample size needed.
- **Distributions** (e.g. result size, shell attempts per intent) need a baseline taken in shadow mode, then a threshold. Thresholds below are starting points [E] to be replaced by your own baseline.

---

## 3. Measurement spec per prioritized mode

### 1a. Invariants

| Mode | Inputs | Definition | Output | Target |
|---|---|---|---|---|
| **C8 context mismatch / silent truncation** | `W` = window the backend actually uses (llama.cpp `/props.n_ctx`; Ollama configured `num_ctx`; hosted: model limit). `E` = your estimate of assembled prompt tokens. `P` = backend-reported prompt tokens (Ollama `prompt_eval_count`; llama.cpp `prompt_n + cache_n`; hosted `usage` sum) [S: backend docs, see repo-device-profiles.md] | `util = E/W` before send. **Pre-send guard:** if `util > 0.85`, prune before sending; if `E > W`, refuse. **Post-send check:** per model keep a rolling median of `P/E` from calls with `util < 0.7` (the calibration ratio). Flag `truncation_suspected` when `P/E` falls below ~0.8× that median while `util > 0.85`, or when `P ≈ W` and `E > W` | `max_util`, `truncation_suspected` count, `guard_refusals` | 0 suspected truncations; p95 `util` < 0.9 |
| **C2 cache-busting prefix** | Hashes of `tools`, `system`, and the message list per call | A **prefix break** is: this call's `tools` or `system` hash differs from the last call's, or the previous call's message list is not a strict prefix of this one's. Record `break_reason ∈ {tools, system, history_rewrite, none}`. Allowed reasons (compaction, model switch) are tagged and excluded | `prefix_breaks_unexplained` | 0. Plus a **golden test** that assembling context for turn n and n+1 yields a byte-identical shared prefix |
| **T11 ignored-path reads** | Path class of each file/dir tool call (from the ignore set) | Run R1 in **shadow mode first**: count calls whose path falls in the deny set and the tokens they would have returned; later switch to enforcing and count *blocked attempts* | `ignored_path_calls`, `ignored_path_tokens` (shadow), `blocked_attempts` (enforced) | After enforcing: attempts near 0; repeated attempts feed G1 |
| **T4/T3 unbounded output, whole-file reads** | Result bytes and estimated tokens per tool call | `over_cap` = result tokens > cap (start at ~5% of `W` [E]); record whether the result was truncated and whether a ranged follow-up occurred | `over_cap_calls`, `max_result_tokens`, `truncation_followup_rate` | p95 result < cap; a ranged follow-up after truncation shows the cap is right |
| **E5 Windows breakage** | Fixtures, plus error-class tags in production | **Tests, not telemetry**: path round-trip through `fs::canonicalize` (`\\?\` forms), CRLF read/write, containment check with mixed separators. In production, tag `ToolError::Io`/`NotFound` whose path contains `\\?\` or mixed separators | test pass/fail; `path_form_errors` | Tests green; production count 0 |

### 1b. Distributions (require `run_shell`)

| Mode | Definition | Output | Target |
|---|---|---|---|
| **E1 command guessing** | Classify each shell call as `spawn_failed` (the argv-spawned executable is missing; in Rust a missing binary surfaces as a spawn `NotFound` error, not an exit code), `ran_nonzero` (ran and failed, which is legitimate for tests), or `ok`. Map argv0 + subcommand to an intent (`build`, `test`, `check`, `fmt`) via a fixed table. Count attempts per intent until first success | `spawn_failed`, `attempts_before_first_ok[intent]`, `profile_hit` (did the first command equal the confirmed profile command) | In a repo with a confirmed profile: 1 attempt per intent; `profile_hit` ≥ 90% |
| **T10 repeated build/test** | Same command hash re-run with **no intervening write** (deterministic); and output tokens per run | `rerun_no_change`, `avg_output_tokens[intent]` | `rerun_no_change` ≈ 0; output capped |

An ambiguity to handle explicitly: a failing test run is not a failure of E1. Only `spawn_failed` and "wrong subcommand" (an ecosystem-specific usage error) count; anything unrecognized is recorded as `ambiguous` and excluded rather than guessed.

### 1c. Measure only

| Mode | Definition | Output | When to act |
|---|---|---|---|
| **C3 cold-cache gaps** | `gap = start(n) − end(n−1)` vs. the backend's TTL (hosted 5 min default / 1 h option [S]; Ollama `keep_alive` default 5 min [S]). `expected_cold` if gap > TTL; `unexpected_cold` if gap < TTL but cache read is near zero or `load_duration` is large (this points to C2 or eviction) | Gap histogram; `expected_cold`, `unexpected_cold`; cold penalty (extra seconds or tokens vs. the warm baseline) | Propose keep-alive or 1 h TTL only when a meaningful share of gaps falls between the default TTL and the longer one **and** the penalty is material |
| **E4 throughput** | `prefill_tok_s = uncached_prompt_tokens / prompt_eval_duration`; `gen_tok_s = eval_count / eval_duration`; time-to-first-token. Sample only calls with ≥ 500 uncached prompt tokens to reduce noise; keep rolling p10/p50 per (model, backend) | Rolling medians | Use to set per-turn prefill budgets only after the distribution is stable (≥ ~50 sampled calls) |
| **P1/P3/P5 protocol errors** | Count per model: JSON parse failure, unknown tool name, tool call emitted as text. Requires splitting today's `InvalidArguments`, which conflates bad args and unknown tool [M] | Rates with 95% intervals | ≥ ~280 calls (~20 sessions) per model for ±3 points at ~7% |
| **E2 repeated exploration** | Hash of the first N tool calls per session; count repeats across sessions in the same repo | `opening_repeat_count` | Build a repo map only if repeats are frequent and the opening's token cost exceeds the map's cost (earlier break-even: a ~2k-token cached map ≈ $0.008/session on Sonnet 5.5 [E]) |

---

## 4. Known measurement risks

| Risk | Handling |
|---|---|
| **Token estimates are inexact** (local tokenizer vs. model's real tokenizer) | Use the calibration ratio `P/E` per model rather than trusting `E` absolutely. Margins (0.85 guard) are deliberately conservative. Prefer reported `P` over estimates whenever available. |
| **Backends may report prompt tokens differently when a cache hit occurs** | **Unverified:** whether Ollama's `prompt_eval_count` counts all prompt tokens or only newly evaluated ones on a cache hit. Docs describe `prompt_eval_duration` as time for *uncached* tokens, while `prompt_eval_count` is described as tokens in the prompt [S]. The C8 post-send check must be validated against a real Ollama run before it is trusted. Until then rely on the pre-send guard. |
| **Gateways may drop fields** | If LiteLLM omits timing fields, call native endpoints for local models. This is the first thing to verify when the client exists. |
| **Detector false positives** | Ship every detector in **shadow mode** (log findings, take no action). Fixture tests include negative cases (e.g. a legitimate re-read after a write is not T1). Before a detector triggers an action or a user prompt, spot-check ~20 flagged findings by hand and require ≥ 90% to be real. |
| **Goodhart on the detectors themselves** | Fixes target the invariant (e.g. keep the prefix stable), never the metric (e.g. do not hide breaks by hashing less). Pair "tokens saved" with the session outcome label once it exists. |
| **High-cardinality metrics** | Mimir labels limited to `backend`, `model`, `tool`, `finding`. Paths, repo ids and session ids live in SQLite and traces only. |

---

## 5. Order of work and gates (no experiments, only build-time checks)

1. **Fields first.** Make the LLM client return `usage`, backend timings, and finish reason through one optional `BackendStats` struct; make tool results carry bytes, estimated tokens, duration, error class, truncated flag.
2. **Detectors as pure functions with fixture tests**, including negative cases. Gate: all fixtures green.
3. **Shadow mode.** Detectors log findings only.
4. **Golden tests** for prefix stability (C2) and Windows path/CRLF handling (E5).
5. **Enforce the invariant fixes** (pre-send guard, deny-read set, caps) once their detectors show ≥ 90% precision on a spot-check.
6. **Distributions** (1b, 1c): collect baselines; act only past the sample thresholds listed above.

Gate to move from 1a to slice 2: zero unexplained prefix breaks and zero suspected truncations across the shadow-mode sessions, and the field coverage targets from the earlier report (≥ 95% of LLM calls with full usage; ≥ 99% of tool calls with outcome class, bytes, duration).

---

## 6. Storage decision

Confirmed facts live in the harness's own data directory (per machine, outside any repo), keyed by repo root path plus manifest hash and by backend + model id. A file inside a repo may be read only as a *suggestion* and is never trusted or executed without a fresh confirmation. This matches the security rules in [repo-device-profiles.md](repo-device-profiles.md) §6.

## 7. Open questions

1. **Calibration source for `E`:** is an approximate local tokenizer acceptable (with the `P/E` calibration), or do you want to avoid a tokenizer dependency and rely only on backend-reported counts plus the pre-send guard?
2. **Shadow period:** are you comfortable running with detectors in log-only mode for the first ~30 sessions before enforcing anything?
3. **Session outcome label:** still undecided. It is not needed for slice 1a, but it is needed before judging any "tokens saved" claim.
