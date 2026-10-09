# Repo and device profiles: feasibility, ROI and design

Date: 2026-10-09. Status: draft for review, analysis only, nothing implemented.
Scope: the **repo** and **device** failure modes from [failure-modes.md](failure-modes.md). The user-intent group (U1–U7) is deferred.
Labels: **[M]** measured from repo/spec, **[S]** stated by a cited source, **[E]** my estimate (given as a range).

---

## 1. Idea in one paragraph

Keep two small, human-readable profiles. A **device profile** (per machine and backend and model) holds measured facts such as usable context size, prefill speed, keep-alive needs, and tool-call reliability. A **repo profile** (per repository) holds detected facts such as build/test commands, paths to ignore, and a cached layout. Facts are **measured or detected by plain code** at near-zero token cost. When a fact needs the user's authority, such as running a command or changing a limit, the harness **proposes it with the evidence and the user confirms once**. Nothing is silently applied except safe, reversible defaults. No LLM reads session transcripts for any of this.

Your point about asking the user is the key safety valve here: confirmation turns a guess into a fact, and it removes the main risk of auto-adaptation (acting on a wrong inference).

---

## 2. Current-code facts that matter [M]

| Fact | Consequence |
|---|---|
| `read_file` uses `fs::read_to_string` on the whole file; no size cap, no line range (`tools/read_file.rs`) | A large file or a lock file goes straight into context (T3, T4). |
| `list_dir` is flat, no ignore filtering (`tools/list_dir.rs`); `target/` exists in the working tree | Listing and reading build output is possible (T11). |
| `Workspace::resolve` canonicalizes with `fs::canonicalize` (`tools/workspace.rs`) | On Windows this returns extended-length (`\\?\`) paths. Containment still works because both sides are canonical, but any path shown to the model or compared with git output needs normalizing (E5). |
| No `run_shell`, so no command discovery exists yet | The repo profile's command part has nothing to attach to until `run_shell` and the allowlist exist. |
| LLM client has no `usage`, no timings, no finish reason (`llm/mod.rs`) | Every device measurement below needs the client to surface backend timing fields first. |
| README says the LLM client is likely to go through LiteLLM | LiteLLM sits between you and Ollama/llama.cpp. **Unverified:** whether it passes through Ollama's timing fields and llama.cpp's `timings`. This decides whether device measurement works through the gateway. See open question 1. |

---

## 3. Device profile

### 3.1 What backends actually expose (so detectors are real, not hypothetical)

| Backend | Exposes [S] | Gives you |
|---|---|---|
| **Ollama** `/api/chat` final object | `load_duration`, `prompt_eval_count`, `prompt_eval_duration` (documented as time evaluating *uncached* prompt tokens), `eval_count`, `eval_duration`, `total_duration` [1] | Cold-load detection, prefill speed, generation speed, and a direct cache-effect signal. |
| **Ollama** config | Default context length 4,096; changed via `OLLAMA_CONTEXT_LENGTH` or per-request `num_ctx`. Models unload after 5 min by default; `keep_alive` / `OLLAMA_KEEP_ALIVE` overrides. `OLLAMA_NUM_PARALLEL` multiplies context memory. `ollama ps` shows GPU/CPU split. `OLLAMA_KV_CACHE_TYPE` can quantize the KV cache [2]. | The knobs the profile would recommend. The docs I fetched do not say what Ollama does when a prompt exceeds `num_ctx` (see D1). |
| **llama.cpp server** | `/props` returns `n_ctx`; responses carry `timings` including `cache_n` (tokens reused from cache), `prompt_n`, `predicted_n`; `--parallel` **divides** the context among slots; `--metrics` exposes Prometheus metrics incl. `llamacpp:n_tokens_max` [3]. | Exact configured context, cache reuse per call, and a ready-made Mimir scrape target. |
| **Hosted API** | Per-call usage incl. cache read/write tokens; TTL 5 min default or 1 h [4]. | Cache health in dollars rather than seconds. |

Hardware inspection (RAM/VRAM sniffing) is deliberately **not** part of this design. Measure behavior instead (speed, load time, truncation), which is portable across backends and doesn't need OS-specific code.

### 3.2 Device-side items

For each: signal, adaptation, user confirmation, and a first-pass assessment. Cost figures are [E].

| ID | Mode (from failure-modes.md) | Signal | Adaptation | Ask the user? | Value | Effort | Feasibility |
|---|---|---|---|---|---|---|---|
| **D1** | Context mismatch and **silent truncation** (C8) | Backend-reported context (`/props.n_ctx`, or configured `num_ctx`) vs. your own token estimate of the assembled prompt. If Ollama's `prompt_eval_count` is at or near `num_ctx` while your estimate is larger, the prompt was cut. *(Inference: the docs don't state the truncation behavior; this check works regardless of the behavior.)* | Set the context-assembly budget to ≈85% of the real window; refuse to send a prompt that cannot fit; trigger pruning before truncation. | Yes, once: "Backend window is 4,096. Recommend `num_ctx` 16,384 (uses ~X GB more memory). Apply?" | **High.** A 4,096 default with a tool-calling agent is easily exceeded after a few file reads, and the failure is silent. The cost is wrong answers, not tokens. | Low (8–16 h) | **High** (deterministic) |
| **D2** | Cold-cache gaps and cold model loads (C3) | Ollama `load_duration` large; gap since previous call vs. keep-alive; hosted `cache_creation` spike after idle > TTL | Recommend longer keep-alive (local); 1-hour TTL for hosted only if the write premium (2× vs 1.25× base input) is repaid, i.e. reads within the hour ≥2 per prefix [4] | Yes (changes memory residency or cost) | Medium-high for local (reload + full re-prefill dominates latency); low-medium for hosted | Low (4–10 h) | High |
| **D3** | Prefix/cache-busting (C2), measured on the device | Hash of `(tools, system, history[..n])` vs. previous call; local: `prompt_eval_duration` high despite stable prefix; llama.cpp `cache_n` low | Fix is static: keep prefix byte-stable, append-only history. The detector is a **regression test and a health metric**, not an adaptive feature | No | **High.** Cheap to prevent, expensive to miss. Illustration [E]: re-prefilling a 30k-token prompt at 200–2,000 tokens/s is 15–150 s. If every turn of a 15-turn session missed the cache, that is ~4–38 minutes of added wait. | Low (6–12 h) | High |
| **D4** | Throughput limit (E4) | Prefill tokens/s, generation tokens/s, time-to-first-token, per model | Derive a **per-turn prefill budget**; make pruning more aggressive on slow machines; warn when a task shape exceeds it | Show, don't ask. Optional: "this model prefills at ~300 tok/s; limit tool output to N?" | Medium (changes thresholds, not correctness) | Medium (12–24 h) | High for measurement, medium for useful adaptation |
| **D5** | Parallel slots shrink the window (llama.cpp) | `/props.n_ctx` vs. `--ctx-size` ÷ `--parallel` | Warn: "effective per-request window is N, not M" | Yes (config advice only) | Medium; easy to hit, easy to miss [3] | Very low | High |
| **D6** | Slow tools/timeouts (E3) | Tool duration percentiles per machine; timeout counts | Calibrate per-tool timeouts from observed p95 × margin | Not needed for increases within a cap | Low-medium (a hung `cargo test` costs time, not tokens) | Low | High, but **defer**: generous static defaults suffice until timeouts actually occur |
| **D7** | Windows-specific breakage (E5) | Error class tags: path form, CRLF, shell/command-not-found | **Not adaptive.** Golden tests for path normalization and CRLF round-trips; `run_shell` is argv-based per spec so shell quoting differences largely vanish | No | Medium (you are on Windows 11) | Low (tests) | High |
| **D8** | Model protocol reliability (P1, P5) | Per-model: malformed-JSON rate, text-instead-of-call rate, unknown-tool rate | Per-model prompt variant or tool-format fallback; routing away from a model that is bad at tool calls. Threshold-based, needs **≥~280 calls ≈ 20 sessions per model** for ±3-point intervals (earlier sample-size math) | Yes (switching model or prompt) | Medium-high **if you use small local models** (the spec warns about them); ≈0 if one frontier model | Medium (12–30 h) | Medium. Log now, act later |

### 3.3 What is measurable per device without any user effort

D1 (via a startup probe), D2, D3, D4, D5. These need **no sessions of history**: they work from the first call, because they compare a call against its own backend's reported limits.

---

## 4. Repo profile

The profile is keyed by repository root plus a content hash of the manifest files, so it is invalidated when the repo changes shape.

| ID | Mode | Signal / detection | Adaptation | Ask the user? | Value | Effort | Feasibility |
|---|---|---|---|---|---|---|---|
| **R1** | Ignore set and read caps (T11, T3, T4) | Static rules, not learning: respect `.gitignore` plus a built-in deny-for-read list (`target/`, `node_modules/`, `.git/`, lock files, build output); size/line caps with an explicit "truncated, N more lines, use range X-Y" marker | Apply in `list_dir`/`read_file`. The `grep` tool, if it shells out to ripgrep, already skips gitignored, hidden and binary files by default [5]. `list_dir`/`read_file` do not, so they must be made consistent | No (safe default; override by config) | **High and certain.** One accidental read of `target/` or a lockfile is thousands of tokens of zero value | Very low (3–8 h) | **Very high** |
| **R2** | Command discovery (E1, T10) | Detect ecosystem from manifests: `Cargo.toml` → `cargo build/test/clippy/fmt`; `package.json` scripts; `Makefile` targets; `pyproject.toml`. Candidate list ranked by prior | Seed the `run_shell` allowlist and a "how to verify" hint with the confirmed commands. The system prompt gets a **two-line** repo facts block (stable, so it stays in the cached prefix) | **Yes, once per repo** (also a security step, see §6) | **High.** Guessing wrong costs a failed call, an error dump and a retry every session; confirming once removes it permanently. Also enables the "ran tests before declaring done" check later (U5) | Low-medium (8–20 h) | High |
| **R3** | Rediscovering the repo each session (E2) | Cross-session: first-N tool-call sequence hash repeated in ≥k sessions of the same repo | A cached repo map keyed by git tree hash (cf. Aider's repo map [6]) injected into the prefix | Yes (adds standing context) | Unclear. Trade-off: a ~2k-token map costs about $0.008/session on Sonnet 5.5 when cached (one write at $2.50/MTok + ~15 reads at $0.10/MTok [4] [E]) but also permanently raises fixed overhead (C4) and goes stale. Worth it only if exploration costs more than that per session, which depends on the repo | Medium (15–35 h) | Medium. **Measure E2 first (cheap D1 query), then decide** |
| **R4** | Pre-existing/flaky test failures (E6) | Baseline test run per commit, store failing set | Distinguish "this change broke X" from "X was already failing" | Yes (costs one extra test run) | Medium for big suites; low for small ones | Medium | Medium. **Defer** |
| **R5** | Exploration without action (L4) | Turns since last write/test above a threshold scaled by repo size | A nudge or a forced "state your plan" turn | No | Low-medium; mostly a loop-control guard | Low | High, but **defer**; belongs with orchestrator loop-control, not profiles |

---

## 5. Ranking by value ÷ effort, then feasibility

Values are qualitative on purpose: there are no sessions yet to put numbers on. Only D3 and R3 carry a rough dollar or time figure.

| Rank | Item | Why | Verdict |
|---|---|---|---|
| 1 | **R1** ignore set + read caps | Tiny, deterministic, large downside avoided; not really "analysis", it is tool design | **GO now** (do with the tool subsystem) |
| 2 | **D1** context probe + truncation guard | Highest-severity device failure, silent, and detectable from the first call | **GO** (with the LLM client) |
| 3 | **D3** prefix stability test + cache-health metric | Cheap to build in, expensive to retrofit; protects everything else | **GO** (with context manager and telemetry) |
| 4 | **R2** command discovery + confirm | Highest-frequency repo failure; also feeds the permission allowlist | **GO** (with `run_shell` and the permission gate) |
| 5 | **D2** keep-alive / TTL advice | Cheap once D1/D3 fields exist | **GO**, small |
| 6 | **D5** parallel-slot warning | Trivial once `/props` is read | **GO**, small |
| 7 | **D7** Windows golden tests | Tests, not features | **GO** (as part of the test suite) |
| 8 | **D4** throughput-derived budgets | Measurement is easy; useful adaptation needs real sessions | **LOG now, adapt later** |
| 9 | **D8** per-model reliability | Needs ~20 sessions per model | **LOG now, defer action** |
| 10 | **R3** cached repo map | Might cost more than it saves | **DEFER**, decide after measuring E2 |
| 11 | **D6, R4, R5** | Low value until the problem is observed | **DEFER** |

Rule of thumb behind the ranking: items that are **deterministic, need no history, and prevent a silent failure** come first; items that need accumulated sessions to be trustworthy come last.

---

## 6. The "ask the user" loop

Principles:

1. **Propose with evidence, one question at a time.** "I tried `make test` (not found), then `cargo test` (passed). Remember `cargo test` as this repo's test command?" Show the observation, not a conclusion.
2. **At most one proposal per session**, at a natural pause (session start for probes, session end for observations), never mid-task.
3. **Defaults are safe and reversible.** Nothing that executes code or changes memory use is applied without a yes. Safe read-side defaults (R1) apply silently but are visible in the profile.
4. **Every fact records its evidence and date**, and can be edited or deleted as a plain file.
5. **Decline is remembered**: don't re-ask the same question unless the evidence changes.

Sketch of the files (design only; names are placeholders):

```toml
# ~/.config/lightning/device.toml   (per machine; never committed)
[backend.ollama.qwen-coder-32b]
confirmed_num_ctx   = 16384        # evidence: truncation seen at 4096 on 2026-10-14
keep_alive          = "30m"
prefill_tok_s       = 410          # measured, rolling median
tool_call_json_fail = 0.07         # measured, rolling

# <repo>/.lightning/profile.toml   (per repo; committed or kept local, see open question 3)
[commands]
test  = ["cargo", "test"]          # confirmed by user
build = ["cargo", "build"]
[paths]
deny_read = ["target/", "*.lock"]
manifest_hash = "..."              # re-detect when this changes
```

**Security rules for repo-sourced profile data (untrusted by default):**

- A profile file inside a repo is **data the repo author controls**. A cloned repo could ship a `profile.toml` that declares `test = ["curl", ...]` or widens `deny_read`. Rule: commands from a repo-resident profile are **never executed or allowlisted without a fresh user confirmation on this machine**. Prefer storing confirmed facts outside the repo (keyed by repo path + manifest hash), and treat an in-repo file only as a *suggestion*.
- Running a repo's own `npm test` or `make test` runs code the repo author wrote. That is the same trust you extend when you run it yourself, and the confirmation prompt should say so in one line.
- Detected facts are drawn from a **fixed ecosystem map** (manifest filename → known command shapes), not from free text in the repo. Script *names* may be offered; script *bodies* are never shown to the model as instructions.
- Profile contents that reach the model are enums, numbers and fixed templates, consistent with the earlier injection rule.

---

## 7. What the loop must log to support all of this

Subset of the signal table in failure-modes.md, restricted to repo+device. **New** means not in the spec's planned schema or in the current code.

| Signal | Used by | Status |
|---|---|---|
| Backend-reported context window; configured context; effective per-request window | D1, D5 | **new** |
| Per call: backend timing fields (`load_duration`, `prompt_eval_*`, `eval_*`; llama.cpp `timings`), time-to-first-token | D2–D4 | **new** (client doesn't return them today) |
| Prefix segment hashes per call; cache read/write tokens | D3 | **new** |
| Idle gap since previous call to the same backend | D2 | **new** (derivable from timestamps) |
| Malformed-JSON / schema / unknown-tool / text-instead-of-call counts per model | D8 | **new** (error types conflated today) |
| Tool duration, result bytes, error class | D6, R1, R2 | **new** |
| Shell: argv0, subcommand, exit code, "command not found" flag | R2 | needs `run_shell` |
| Manifest hash, git head, OS | R-series, D7 | **new** |

Design note: pass backend timing through the abstraction as one optional struct (`BackendStats`) on the response, so hosted and local backends fit the same field.

---

## 8. Risks specific to this scope

| Risk | Mitigation |
|---|---|
| **Gateway hides backend fields** (LiteLLM may not forward Ollama/llama.cpp timings) | Verify early (open question 1). Fallback: call the backend's native endpoint directly for local models; keep LiteLLM for hosted. |
| **Stale profile** (repo changed shape, model swapped, driver updated) | Key by manifest hash and model id; store the date; re-probe cheaply at session start. |
| **Wrong auto-detection of commands** | Ranked candidates + user confirmation + record the fallback that actually worked. |
| **Over-engineering** | R1, D1, D3, R2 are most of the value. Everything else is gated on observed need. |
| **Measuring on a noisy machine** (other apps using GPU/CPU) | Use rolling medians, ignore outliers, never auto-apply a speed-derived setting without confirmation. |
| **Silent-truncation inference is unverified** | The D1 check (compare your estimate to the backend-reported count) detects truncation without relying on documented behavior. |

---

## 9. Open questions for you

1. **Gateway:** will local models go through LiteLLM, or directly to Ollama/llama.cpp? This decides whether D1–D5 can see backend timings. It is worth a 30-minute check once the client exists.
2. **Primary local runtime and model class:** Ollama or llama.cpp, and roughly what model size and GPU/CPU? D1–D5 matter far more for local than hosted, and D8 only matters for small models.
3. ~~Where should confirmed repo facts live?~~ **Decided (2026-10-09):** at the harness level, outside any repo, keyed by repo root path + manifest hash and by backend + model id. A file inside a repo is read only as a suggestion and never trusted or executed without fresh confirmation.
4. **How much asking is acceptable?** At most one proposal per session is my default. If you'd rather have zero prompts and a printed report instead, R2 and D1 would become "recommend in the session summary" instead of a prompt.
5. **Ordering relative to the build:** R1, D1, D3 and R2 each attach to a subsystem that doesn't exist or doesn't compile yet (tools, LLM client, context manager, `run_shell`). Should I treat these as **requirements on those subsystems** (fields and checks to design in) rather than a separate "profiles" milestone? I recommend yes.

---

## Sources

1. [Ollama API docs](https://github.com/ollama/ollama/blob/main/docs/api.md): `/api/chat` timing fields and `keep_alive`.
2. [Ollama FAQ / configuration](https://docs.ollama.com/faq): default context length 4,096, keep-alive, parallelism, KV cache type, `ollama ps`. The docs state no truncation behavior.
3. [llama.cpp server README](https://github.com/ggml-org/llama.cpp/blob/master/tools/server/README.md): `/props`, `timings`, `--parallel`, `--metrics`.
4. [Anthropic prompt caching](https://platform.claude.com/docs/en/build-with-claude/prompt-caching) and [pricing](https://platform.claude.com/docs/en/about-claude/pricing): TTLs, write/read multipliers, Sonnet 5.5 prices.
5. [ripgrep](https://github.com/BurntSushi/ripgrep): respects gitignore, skips hidden and binary files by default (confirmed via search-result summaries; **secondary**).
6. [Aider repository mapping](https://deepwiki.com/Aider-AI/aider/4.1-repository-mapping-system) (**secondary**).

Not verified: LiteLLM's pass-through of backend timing fields; Ollama's current default context length in newer releases than the docs I fetched; whether Ollama exposes the `ollama ps` information through an HTTP endpoint.
