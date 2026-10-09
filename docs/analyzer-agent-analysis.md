# Analyzer agent: feasibility and ROI for `lightning`

Date: 2026-10-09. Scope: analysis only, nothing implemented.

**Labels used throughout.**
**[M]** = measured from the repo, the spec, or a local file I inspected.
**[S]** = stated by a cited source.
**[E]** = my estimate or assumption, always given as a range.

---

## 0. Executive summary

1. **Don't build an analyzer *agent* now. Build the data layer and an LLM-free report.** The components are worth very different amounts. Component 1 (token report) and the stats half of 3 (per-model profiles) are cheap, deterministic and teach you the LGTM stack. Component 2 (skill mining) is a defer. Component 4 (preference inference) is a no-go.
2. **At single-user volume the dollar ROI is negative for every component once your hours are priced.** My central estimate is $10–$120/month of API spend on Sonnet 5.5, and about $0 on a local model [E]. Break-even spend at $25/h with learning credit ranges from about $440/month for Component 1 to about $18,000/month for Component 4 (§B.4). The analyzer's own LLM cost is about $1/month and is not the issue. Your build and maintenance hours are.
3. **The project cannot produce analyzable data yet.** [M] The `lightning` lib does not compile: `cargo check` gives 12 errors, all in `llm/`. There is no orchestrator, session store, permission gate, or `write_file`/`run_shell`/`grep`. `tracing` is declared as a dependency but never called (0 uses). You have 0 sessions. The analyzer is gated behind about four other subsystems, and that is the most important fact in this report.
4. **Two of your settled design ideas need amending (§D.4).** (a) "Replay past sessions" cannot measure success or token change, because the model's behaviour depends on its context and replay cannot reproduce that. (b) "Metadata only" is not enough, because file paths, command strings and error messages are also attacker-controlled.
5. **Cheapest kill-or-validate experiment (§D.3):** write a roughly 150-line script over the 87 Claude Code transcripts already on your machine (141 MB, 8 projects, since 2026-08-28 [M]). It measures re-reads, tool-output share, repeated tool n-grams and corrections. It takes about 6 hours and costs $0. If no waste category exceeds 10% of tokens and no sequence repeats in at least 3 projects, drop Components 2 and 4 permanently.

---

## 1. Ground truth: what the repo contains today [M]

| Area | Spec says | Code says |
|---|---|---|
| Tools | `read_file`, `write_file`, `list_dir`, `grep`, `run_shell` | Only `read_file` and `list_dir` (`tools/registry.rs`). The registry test body is empty (`registry.rs:90`). |
| Tool result | `content` + open `metadata` (bytes, exit code, elapsed) | `ToolOutput {content, metadata}`. `read_file` emits only `line_count` (`read_file.rs:26-31`). `list_dir` emits only `item_count`. No bytes, token estimate, duration or content hash. |
| Errors | NotFound, PathOutsideProject, InvalidArguments, ExecutionFailed, Timeout, Io | `ExecutionFailed` and `Timeout` are commented out (`error.rs:9-10`). `InvalidArguments` conflates "bad JSON args" and "unknown tool name" (see message text). |
| LLM client | Typed `ChatMessage` with `tool_call_id`, `ChatResponse` enum, retries | `ChatMessage {role: String, content: String}` (`llm/mod.rs:31`). No `tool_call_id`. `LLMResponse` has no `usage` field. The client does not compile (`from_env` fills fields with `()`, `chat` has undefined variables). `openai.rs` is an unfinished sketch. Only the first choice and first tool call are handled, so parallel tool calls would be dropped. |
| Orchestrator | Single loop with iteration cap | None. `main.rs` is a one-shot `inquire` menu. |
| Permission gate | Allow/Deny/AskUser | None. |
| Session store | SQLite `sessions` + `events` | None. No `rusqlite`/`sqlx` dependency. |
| Telemetry | `#[instrument]` spans, OTLP to LGTM, Mimir counters | `tracing`, `tracing-subscriber`, `tracing-appender` are in `Cargo.toml`. No `tracing-opentelemetry`, no `opentelemetry-otlp`, and zero `tracing::`/`#[instrument]` uses in the source. |
| Status per README | | Tools and CLI working. Everything else "not yet". |

**Implication.** Every question of the form "what does my current schema capture?" has the answer "nothing yet, but the *planned* schema is partly sufficient." §A.1 compares the planned schema in the spec against each component's needs.

The spec already contains a related idea: §13 "Self-Monitoring (Ops) Agent" queries Mimir/Loki for anomalies such as "tool X failed 40% of the time." Your proposal is the offline, cross-session, heavier version of that. Treat it as an extension of §13, not a new subsystem.

---

## A. Feasibility

### A.1 Data requirements and gaps, per component

"Planned" means the spec's `events` table (`session_id, turn_index, kind, payload_json, created_at`), the `Turn` enum, and the span attributes listed in §11 of the spec.

| # | Component | Fields it needs | Planned schema covers | Gaps (must add) |
|---|---|---|---|---|
| 1 | Token efficiency | Per LLM call: `input_tokens`, `output_tokens`, **`cache_read_tokens`, `cache_write_tokens`**, finish reason, latency, model. Per tool result: byte size, token estimate, truncated flag. Per `read_file`: normalized path, **content hash**, file mtime or hash at read time. Context breakdown per call (system, tools, history, tool-result tokens). Compaction or truncation events. | `prompt_tokens`, `completion_tokens`, `latency_ms`, `model` on the `llm.call` span. Full tool args and output inside `payload_json`. | Cache read and write tokens (the spec has none, and Anthropic reports them separately [S][1]). Tool-result size and hash. Context-composition breakdown. "File changed between reads" signal. Compaction events. `LLMResponse` must carry `usage` through the client (it does not today). |
| 2 | Repeated sequences → skills | Ordered tool calls per session with **normalized args** (path → glob or role, command → argv0 + subcommand), per-call outcome class, **session task label and outcome label**, timestamps. | Ordered events and args in `payload_json`. | Task type or label. Session outcome (the spec's `status` is only running/completed/failed, which says the agent finished, not that the work was right). Arg normalization is a post-processing job. Without outcome labels you will mine frequent *and failing* sequences. |
| 3 | Per-model profiles | `model` (exact id + backend) on **every** call. Failure taxonomy: JSON-unparseable, schema-invalid, unknown-tool, runtime error, permission-denied. Retry counts. Loop detection (same call hash repeated k times without a new file or hash). Session termination reason (`Completed`, `MaxIterationsReached`, `Failed`). A task-difficulty covariate. | `model` on `llm.call`. `tool.execute` success bool. Outcome enum exists in the orchestrator design. | Error taxonomy (today `InvalidArguments` mixes unknown tool and bad args). Termination reason persisted on `sessions`. Malformed-call flag at the LLM-client layer (`LLMError::BadToolArguments` exists but is not tied to a model). Loop hash. Difficulty proxy (turn count, files touched). |
| 4 | Preference inference | User follow-up messages after an agent action, permission **denials by the user**, edit reverts (pre- and post-image hash of each `write_file`, git state), explicit approvals. | `UserMessage` turns. The permission `Deny` decision. | The spec's `Deny` is a *policy* decision (allowlist), not a user preference. A user-visible `AskUser` prompt is marked "v2", so in v1 there are **no user denials to learn from**. No write pre-image. No git snapshot. No link between a user message and the action it corrects. |

**Cross-cutting fields to add now, because they are cheap and can't be backfilled:**
`schema_version`, `git_head` + dirty-diff hash at session start, harness version, system-prompt hash, tool-schema hash, `model` + `base_url`, `sampling params`, a session outcome label (one keypress at session end), and a `trace_id` stored in SQLite so Tempo spans join to rows.

Use the OTel GenAI conventions for names where they exist. They define cache-token attributes (`gen_ai.usage.cache_read.input_tokens`, `gen_ai.usage.cache_creation.input_tokens`) and `invoke_agent` / `chat` / `execute_tool` operations [S][2]. The page I fetched says the conventions are in "Development"/"Experimental" status, so expect renames and pin a version.

### A.2 Deterministic analytics vs. LLM judgment

| Component | Deterministic (plain code) | Needs an LLM | Verdict |
|---|---|---|---|
| 1 Token efficiency | Everything: re-read rate, bytes per tool, history share, cache-hit ratio, cost per session. | Optional phrasing of recommendations. | **~100% deterministic.** The LLM adds nothing the numbers don't. |
| 2 Skills | Sequence mining (n-grams or PrefixSpan), support counts, normalization. | Turning a pattern into a *written* skill (name, trigger description, steps). | Mining deterministic. Authoring needs judgment, and see §A.4 on why an LLM writing from logs is the risky part. |
| 3 Model profiles | All rates, intervals, per-tool breakdowns. Routing rules (if model X's malformed-call rate > threshold, fall back). | Prompt-tweak suggestions. | **Stats deterministic.** Prompt tuning is the only LLM-shaped piece, and it is low-evidence (§C.3). |
| 4 Preferences | Counting denials and reverts. | *Classifying* a user message as a correction or a preference, then generalizing it into a rule. | The valuable part is the LLM part, and it is the noisiest and most injection-exposed. |

### A.3 Technical risks

| Risk | Where it bites | Quantification [E unless noted] |
|---|---|---|
| **Sparse data** | Components 2 and 4 especially | A single user running 10–100 sessions/month, heterogeneous tasks. Pattern support ≥5 sessions across ≥3 contexts likely needs 150–300+ sessions on recurring task types. |
| **Variance / sample size** | Any before/after claim | See table below. Live A/B on one user is infeasible for small effects. |
| **Attribution** | All "did my change help?" claims | Confounded by task mix, model upgrades, provider changes. Provider-side changes already happen: Sonnet 5.5 cache reads were repriced to 0.05× base [S][1], and Claude 4.7+ models use a tokenizer producing ~30% more tokens for the same text [S][3]. A "savings" series that spans those events is meaningless without model and price version columns. |
| **Non-determinism in replay** | The "replay past sessions" validation | A replayed session only reproduces the tool results if the filesystem state is restored. The model's *next action* depends on the context it sees, so changing the context changes the trajectory. Replay can answer "how many tokens would this rule have removed from the recorded context" (counterfactual accounting). It cannot answer "would the agent still have succeeded." |
| **Proxy gaming (Goodhart)** | Tests-pass as quality metric | Agents can edit tests or special-case checks; reward hacking in long-horizon coding agents is an active benchmark topic (e.g. SpecBench [S][14]). Use a held-out check the agent can't edit. |
| **Trace integrity** | Anything that reads logs written by the agent's own process | Agents with access to their own execution records can alter them [S][15]. Keep the store write-only from the agent's tool sandbox (it is already outside `Workspace`, keep it that way). |
| **Observability backend mismatch** | Proposal says "optionally Loki/Tempo/Mimir" | Those are built for dashboards and short-retention debugging, not for sequence mining or joins. Use SQLite as the source of truth; use LGTM for dashboards. [E: engineering judgment] |

**Sample sizes needed (two-sided α=0.05, power 80%, computed by me with the standard two-sample formulas):**

| What you want to detect | Assumption | n per arm |
|---|---|---|
| Success-rate change 60% → 70% | binary | **376** |
| Success-rate change 60% → 80% | binary | 94 |
| Token reduction of 15% | per-session coefficient of variation 0.5 | **174** |
| Token reduction of 15% | CV 0.3 (e.g. fixed task suite) | 62 |
| Token reduction of 30% | CV 0.5 | 43 |
| Malformed-call rate of ~7% to ±3 pts (95% CI) | per tool call | 277 calls (~20 sessions/model) |

At 30–100 sessions/month, a live A/B of one change on token usage (CV 0.5, 174 per arm, ~350 sessions total) takes about 3.5–12 months, and a success-rate A/B (~750 sessions total) is out of reach. **Conclusion: use a fixed task suite run on demand, not live traffic, to evaluate changes.**

### A.4 Security

| Threat | Mechanism in your design | Mitigation |
|---|---|---|
| **Prompt injection via logs** | `read_file` returns repo content; `grep` returns matches; shell output returns whatever a program prints. All of it lands in events. An LLM that later reads those events can be steered. | Your "metadata only" rule is the right start but not sufficient. **File paths, filenames, command strings, argv and error messages are also attacker-controlled** (a repo can contain a file named like an instruction). Rule: the LLM only receives *enum values, counts, hashes, and your own template strings*, never raw strings from the session. |
| **Poisoned persistent skills** | Memory and skill poisoning is a recognized class (OWASP ASI06 listed in 2026; MINJA reports >95% injection success under idealized conditions) [S][13]. A skill is executable-ish context that persists across every future session. | Human writes or edits the skill body. The LLM may only *point at a pattern* ("sequence S occurred in 14 sessions, 12 succeeded"). Version skills in git, diff before accept, cap skill count (§C.3: more skills hurts). |
| **Secrets in logs** | `read_file` on `.env`, `run_shell` printing tokens, API keys in env echoed to output. Logs are copied to SQLite, then Loki/Tempo (searchable), then maybe an LLM. | Redact at write time (regex + entropy filter), deny-list paths (`.env`, `*.pem`, `id_*`) in the permission gate, store tool output as **hash + size + short excerpt** rather than full text by default, and keep full payloads in a separate, optional, local-only table. |
| **Analyzer as a new privileged writer** | It proposes changes to prompts and skills that every session loads. | Proposals are data, applied only by a human via a reviewed commit. No tool in the analyzer's tool set writes into the skill or prompt directories. |

### A.5 Effort: Rust vs. Python sidecar

All hours are [E], for a Rust learner (not an expert), including debugging. Ranges are low / central / high.

| Item | Rust | Python (DuckDB/pandas over the SQLite file) | Notes |
|---|---|---|---|
| Stage 1: log the right fields (marginal over the spec's planned session store + telemetry) | 8 / 16 / 30 h | n/a | Needed anyway for the §11 spec, so only the *extra* fields count against this proposal. |
| C1 token report | 8 / 16 / 30 h | 4 / 8 / 16 h | Plain SQL aggregates. Python is half the time and a fair choice (you already plan a Python RAG sidecar). |
| C2 mine + propose + approval flow + skill loader in the context manager | 40 / 75 / 150 h | 30 / 55 / 110 h | The skill *loader* (consume side) is mandatory for any value and is harness work. |
| C3a model stats | 6 / 12 / 24 h | 3 / 6 / 12 h | Or Mimir counters + Grafana panels, which is also the stated learning goal. |
| C3b routing + prompt tuning + eval suite | 40 / 70 / 140 h | 30 / 55 / 110 h | Eval suite is the bulk. |
| C4 preference inference | 20 / 40 / 80 h | 15 / 30 / 60 h | Depends on `AskUser` UI (the Tauri GUI) existing. |

Dependencies: `rusqlite` or `sqlx`, `tracing-opentelemetry` + `opentelemetry-otlp`, a token counter (`tiktoken-rs`; for Claude models prefer the provider's reported usage over local counting), and for C2 a sequence-mining routine (hand-rolled n-gram counting is enough; PrefixSpan is overkill at this volume). Python side: `duckdb` reading the SQLite file directly.

---

## B. ROI model

### B.1 How to read this

The model has **three views** because the answer depends mostly on how you value your own hours:

- **View A, cash only:** hours are free. Only API dollars count.
- **View B, hours at $25/h with learning credit:** a fraction of build hours is treated as the project's *goal* (you want to learn Rust and LGTM) and costs nothing.
- **View C, hours at $50/h, no credit:** a pure opportunity-cost view.

The runnable model is in Appendix A. Edit the assumptions table, run it, and the break-even tables regenerate.

### B.2 Assumptions you can edit

| ID | Assumption | Low | Central | High | Basis |
|---|---|---|---|---|---|
| a1 | Sessions per month `N` | 10 | 40 | 150 | [E] single user; your harness competes with Claude Code/Codex for real work |
| a2 | Cost per session on Sonnet 5.5 (cached) | $0.13 (8 turns) | $0.29 (15 turns, 8k→40k ctx) | $1.13 (40 turns, 10k→100k ctx) | Computed from [S][3] prices: in $2, cache read $0.10, 5m write $2.50, out $10 per MTok; assumes 90% cache hit [E] |
| a3 | Monthly API spend `S = N × a2` | ~$1 | ~$12 | ~$170 | derived. **$0 if you run on Ollama/llama.cpp.** |
| a4 | Build hours C1 / C2 / C3a / C3b / C4 (Rust) | 8/40/6/40/20 | 16/75/12/70/40 | 30/150/24/140/80 | [E], from §A.5 |
| a5 | Maintenance hours per month C1 / C2 / C3a / C3b / C4 | 0.5/2/1/1/2 | 1/4/2/3/4 | 2/8/4/6/8 | [E]: reviewing reports and proposals, fixing drift |
| a6 | Saving as fraction of `S`, C1 / C2 / C3a / C3b / C4 | 2/0/0/0/0 % | 8/2/3/10/1 % | 20/8/10/40/3 % | [E]. See reasoning below. |
| a7 | Analyzer's own LLM cost per month, C2 / C3b / C4 (C1, C3a: none) | $1/2/1 | $3/6/3 | $8/20/8 | Weekly run, 30k–200k tokens in, 3k–20k out on Sonnet 5.5 = $0.09–$0.60/run [E], plus a share for approved-proposal validation |
| a8 | Learning credit (share of build hours that are "the goal") C1 / C2 / C3a / C3b / C4 | | 70/30/70/30/10 % | | [E], judgment about how much new Rust/LGTM material each teaches |
| a9 | Amortization horizon | 6 mo | 12 mo | 24 mo | [E] |
| a10 | Hour value | $0 | $25 | $50 | your choice |
| a11 | Sessions needed before output is trustworthy, C1 / C2 / C3a / C3b / C4 | 15/100/20/80/250 | 30/200/40/150/500 | 60/400/80/300/1000 | [E]; reasoning in §D.2 |

**Why the savings fractions are small (a6).**
- *Caching has already taken the big discount.* On Sonnet 5.5, cache reads are 0.05× base input [S][3]. A 25k-token junk tool output costs about $0.0625 once to write and $0.0025 per later turn to read. Over 10 turns that is roughly $0.09. Removing it saves cents, not dollars [E, my arithmetic].
- *The biggest lever is model choice, not log analysis.* Haiku 5.5 is $0.10/$0.50 per MTok versus $2/$10 for Sonnet 5.5 [S][3]. A 20× price gap dwarfs any context-trimming recommendation. Choosing which tasks can go to the cheap model is a C3b question, which is why C3b has the highest ceiling (up to 40%) but also the highest evidence bar.
- *For a local backend the "saving" is latency and context fit, not dollars.* Then the dollar ROI is ~$0 for everything, and the case rests on learning value.

**Per-session validation is the hidden cost.** Validating one proposal by re-execution (20 sessions × 2 arms × 3 repeats = 120 runs) costs about $30–$72 at $0.25–$0.60/run [E]. That is more than a month of spend at the central volume. This is why re-execution validation does not fit single-user economics unless the backend is local.

### B.3 Per-session cost illustration (Sonnet 5.5, computed)

| Session shape | Uncached | With caching (90% hit) |
|---|---|---|
| 8 turns, context 6k→20k | $0.26 | $0.13 |
| 15 turns, context 8k→40k | $0.81 | $0.29 |
| 40 turns, context 10k→100k | $4.64 | $1.13 |

### B.4 Break-even monthly spend

Break-even spend `S*` is the monthly API spend at which `S × saving% = amortized build + maintenance + analyzer LLM cost`. **Central** assumptions, 12-month horizon.

| Component | View A (cash only) | View B ($25/h, learning credit) | View C ($50/h, no credit) |
|---|---|---|---|
| C1 token report (deterministic) | **always positive** (cost is $0 cash) | **~$440/mo** (best case ~$90, worst ~$3,400) | ~$1,460/mo |
| C2 skill mining + proposer | ~$150/mo | ~$10,600/mo (best ~$1,370, worst never) | ~$25,800/mo |
| C3a model stats | **always positive** | ~$1,900/mo (best ~$290, worst never) | ~$5,000/mo |
| C3b routing / prompt tuning | ~$60/mo | ~$1,830/mo (best ~$210, worst never) | ~$4,480/mo |
| C4 preference inference | ~$300/mo | ~$17,800/mo (best ~$2,950, worst never) | ~$37,000/mo |

"Best" uses low cost and high saving %. "Worst" uses high cost and low saving %, where "never" means the saving fraction is 0%.

**Reading.** Your plausible spend is $1–$170/month (a3). Under any view that prices hours, nothing breaks even on dollars. Under cash-only, C1/C3a cost nothing to run so they cannot lose money, and C3b would pay only if monthly spend exceeds about $60 and routing really saves 10%. The honest conclusion is that this work is justified by learning and portfolio value, not by savings, and that argument is strong for C1/C3a and weak for C2/C4.

### B.5 Sensitivity: months to break-even (View B, central costs) including time to accumulate data

Columns are monthly spend `S` in dollars. Rows are sessions per month `N`. The value is `(sessions needed ÷ N) + build cost ÷ monthly net saving`. "never" means more than 60 months or net saving ≤ 0.

**C1 token report**

| N \ S | $10 | $30 | $100 | $300 | $1,000 | $3,000 |
|---|---|---|---|---|---|---|
| 10 | never | never | never | never | 5.2 | 3.6 |
| 30 | never | never | never | never | 3.2 | 1.6 |
| 100 | never | never | never | never | 2.5 | 0.9 |
| 300 | never | never | never | never | 2.3 | 0.7 |

**C3a model stats**

| N \ S | $10 | $30 | $100 | $300 | $1,000 | $3,000 |
|---|---|---|---|---|---|---|
| 10 | never | never | never | never | never | 6.2 |
| 100 | never | never | never | never | never | 2.7 |

**C3b routing / prompt tuning**

| N \ S | $10 | $30 | $100 | $300 | $1,000 | $3,000 |
|---|---|---|---|---|---|---|
| 10 | never | never | never | never | never | 20.6 |
| 100 | never | never | never | never | never | 7.1 |

**C2 and C4:** "never" in every cell of the grid (spend up to $3,000/mo, sessions up to 300/mo).

Two things to notice. Volume (rows) barely changes the answer once it is above ~30 sessions/month, because the dollar value is driven by `S`. Volume matters for *statistical validity* (§D.2), not for payback. And no cell in the realistic region (S ≤ $170) is positive for any component on dollars.

### B.6 Learning value (the stated goal)

Not monetized. My judgment of what each component teaches:

| Component | Rust / LGTM learning | Comment |
|---|---|---|
| Stage 1 logging | High | Forces the span tree, OTLP export, Loki/Tempo/Mimir correlation. This is what §11 of the spec already requires. |
| C1 report | Medium-high | SQL over your own events, PromQL on Mimir counters, Grafana panels. Rust: `rusqlite`, iterators, serde. |
| C3a stats | Medium-high | Labeled metrics, histograms, cardinality (careful with `model` and `tool` labels), dashboards. |
| C3b routing | Medium | Trait-based backend selection (spec §14 wants this anyway). The eval-suite part is more about method than Rust. |
| C2 skills | Low-medium | Mostly prompt and process work, a skill loader, and an approval UI. Little new Rust or LGTM. |
| C4 preferences | Low | NLP-ish classification and risk management. |

### B.7 Which components are likely not worth it at single-user volume

- **C4: not worth it.** Corrections and reverts are rare (assume 0–2 per session [E]). You could write the same preferences into an `AGENTS.md` in ten minutes, and you are the only user, so there is no population to infer from. Highest injection and poisoning exposure, lowest value.
- **C2 as an automatic miner: not worth it.** The evidence says self-generated skills do not help on average (§C.3) and you won't have recurring task structure at this volume. Hand-written skills for 2–3 workflows you actually repeat are worth it, but that is a documentation task, not an analyzer.
- **C3b prompt-tuning: not worth it before C3a produces real differences between models.** Routing between two backends with a hand-written rule is worth it much earlier, and needs no agent.

---

## C. Prior art and current state

Findings were checked against primary documentation where I could fetch it. Anything resting only on secondary sources (blog posts, GitHub issues, search snippets) is marked **secondary**.

### C.1 What providers and harnesses already solve, so you shouldn't rebuild it

| Capability | What exists | Implication for you |
|---|---|---|
| **Prompt caching (Anthropic)** | Explicit `cache_control` or a top-level automatic mode. 5-minute and 1-hour TTL. Writes cost 1.25× / 2× base input. Reads cost 0.1× base (0.05× on Opus 5.5 and Sonnet 5.5; 0.025× on Fable 5.1). Minimum cacheable prefix is 512 tokens on current models. Up to 4 breakpoints. Order is tools → system → messages; changing tool definitions invalidates everything [S][1][3]. | Biggest single cost lever, and it is a *prompt-ordering discipline*, not an analytics problem. Keep tool definitions and the system prompt byte-stable, append-only history, and log the `usage.cache_*` fields. |
| **Prompt caching (OpenAI)** | Automatic, minimum 1,024 tokens (GPT-5.6+). Reads 0.1× on most models. On GPT-5.6+ cache writes cost 1.25×; earlier models had no write charge. Changing tools, reasoning settings, or context management (compaction) can break prefix reuse [S][4]. | Same discipline. Compaction and cache-friendliness pull against each other. |
| **Prompt caching (local)** | `llama-server` keeps each slot's KV cache and re-prefills only the new suffix by default; `--cache-reuse` can shift and reuse chunks after a trimmed history. Ollama reuses a shared prefix if the model stays loaded; default unload is 5 minutes, so set `keep_alive` [S][5]. **secondary** (community tutorial and guide). | For local models, "token efficiency" = prefill latency. Stable prefixes matter more than trimming. |
| **LiteLLM** (your likely gateway) | Can inject `cache_control` automatically via `cache_control_injection_points`; returns Anthropic cache-creation token counts [S][6]. | You can get caching without custom client code, but make sure the `usage` fields survive the gateway into your events. |
| **Tool-result clearing and compaction (provider-side)** | Anthropic context editing: `clear_tool_uses` (default trigger 100k input tokens, keeps last 3), thinking-block clearing, and server-side compaction; clearing invalidates cached prefixes, with `clear_at_least` to make it worthwhile. Beta header `context-management-2025-06-27` [S][7]. | Directly overlaps C1's "oversized tool outputs / history bloat." A threshold-based policy already exists; your report would tell you *whether it triggers and what it costs in cache*. |
| **Compaction in harnesses** | Claude Code re-injects CLAUDE.md and auto memory after compaction and preserves recent skill invocations within a 25,000-token budget [S][8]. A trigger around 83.5% of the window is reported by community sources (**secondary**). OpenCode prunes old tool outputs beyond roughly the latest 40k tokens before summarizing (**secondary**, from search snippet). | Compaction strategies are commodity. Copy the pattern: prune tool output first, summarize second. |
| **Skills** | `SKILL.md` with frontmatter; descriptions always in context, bodies loaded on invocation (progressive disclosure). Claude Code docs say Claude **cannot create skills automatically**; the user writes them [S][8]. The same `SKILL.md` format is used by Codex CLI, Gemini CLI and Cursor per community sources (**secondary**). | No major harness ships automatic skill mining as a core feature. Adopt the file format so your skills are portable, and write them by hand. |
| **Cross-session memory** | Claude Code has CLAUDE.md (you write) and **auto memory** (Claude writes notes from corrections and preferences; first 200 lines or 25KB loaded each session) [S][9]. Cline's Memory Bank is a manual methodology with markdown files (**secondary**). Cursor's learned "Memories" were reportedly removed in 2.1 in favor of rules and AGENTS.md (**secondary**, single source, verify before relying on it). | C4 is already shipped, in a simpler and safer form, by the harness that the tooling vendor maintains. Where it is shipped, it writes plain notes, not a mined statistical model. |
| **Per-model tuning** | Aider has a per-model settings file: `edit_format`, `weak_model_name`, `editor_model_name`, `reminder` placement, `system_prompt_prefix`, `cache_control` [S][10]. Codex CLI reportedly ships a separate prompt file per model generation (**secondary**). | Per-model tuning in practice = **static, hand-curated config** keyed by model. I found no evidence that these tools tune automatically from traces. (Not verified exhaustively; absence of evidence in a few searches, not proof.) |
| **Repo context** | Aider's repo map plus prompt caching of system prompt, repo map and read-only files [S][11]. | If re-reads are your biggest waste, a cached repo map beats an analyzer. |
| **Telemetry vocabulary** | OTel GenAI semantic conventions (chat, invoke_agent, execute_tool; cache read/creation token attributes; Development status) [S][2]. | Use these attribute names from day one so your Grafana panels are portable. |

### C.2 Pricing snapshot used in this report [S][3] (retrieved 2026-10-09; prices change)

| Model | Input | 5m write | Cache read | Output |
|---|---|---|---|---|
| Opus 5.5 | $4 | $5 | $0.20 | $20 |
| Sonnet 5.5 | $2 | $2.50 | $0.10 | $10 |
| Haiku 5.5 (≤100k prompt) | $0.10 | $0.125 | $0.01 | $0.50 |
| Haiku 4.5 | $1 | $1.25 | $0.10 | $5 |

Per MTok. Batch API: 50% off input and output. Caching and batch discounts stack. Haiku 5.5 bills a higher tier for prompts above 100k tokens. A tool-use system prompt adds about 286 tokens per request on 5.5-generation models.

### C.3 Published results on self-improvement from traces, and failure modes

| Source | Finding | Relevance |
|---|---|---|
| SkillsBench (86 tasks, 7 configs, 7,308 trajectories) [S][12] | Curated skills raised pass rate by **+16.2 points** on average (+4.5 for software engineering, up to +51.9 for healthcare); 16 of 84 tasks got worse. **Self-generated skills: −1.3 points on average**, i.e. no benefit. Focused 2–3 skills beat comprehensive sets (+18.6 vs +5.9 points for 4+). | The strongest direct evidence against C2 as an automatic miner. Software engineering gains the least from skills. Expect the smallest effect in your domain. |
| Self-evolving skills over multiple rounds [S][16] | 388 candidates over 42 runs, 3 models, 5 benchmarks: only 55 produced a new validation best; 11 were selected; **validation-to-test reversals** occurred (+5.7 on validation, −6.6 on test for one case). Improvements came from runs that saw *failed* trajectories; success-only feedback was never selected. | Sparse, noisy, overfits validation. If you mine skills, you need failed sessions in the data, and a held-out test set. |
| GRASP [S][17] | Adding skills causes regression in existing behavior; a gate that checks for regression before accepting is the mitigation. | Supports your "measure before and after" rule, and says the gate must test *old* tasks, not only the target one. |
| ACE (Agentic Context Engineering), ICLR 2026 [S][18] | Reports +10.6% on agent tasks and +8.6% on finance benchmarks. Identifies **brevity bias** and **context collapse** (iterative rewriting erodes detail); mitigates with incremental, append-style updates and a curator. | If you do evolve context, use incremental deltas with human review, never wholesale LLM rewrites of the prompt. Note: these gains are on benchmarks with many runs and a feedback signal. |
| Agent Workflow Memory, Voyager [S][19] | Reusable workflows induced from trajectories help on web-agent and game benchmarks. | Real but demonstrated where tasks repeat many times with an automatic success signal. Your setting has neither. |
| Reward hacking in coding agents (SpecBench) [S][14] | Documented as a pervasive failure on long-horizon coding tasks. | Don't use "tests pass" as the only gate; agents can edit tests. |
| Memory poisoning (MINJA, AgentPoison; OWASP ASI06) [S][13] | Persistent memory is an attack surface that survives across sessions. | §A.4. |

**What the literature does not give you:** evidence that any of this works for a *single-user, low-volume, local coding harness*. The positive results come from benchmark suites with hundreds of repeated tasks and automatic scoring. That is the regime you should expect to need to simulate (a task suite), not the regime you'll organically have.

---

## D. Recommendation

### D.1 Ranking by value ÷ effort

| Rank | Component | Value | Effort | Verdict |
|---|---|---|---|---|
| 1 | **C1 Token-efficiency report** (deterministic, no LLM) | Medium: finds real waste, teaches SQL/Grafana, validates caching works | Low (8–30 h) | **GO**, as a script or Grafana dashboard, after Stage 1 logging |
| 2 | **C3a Per-model stats** (counters + dashboard) | Medium-high *if* you use ≥2 backends (the spec plans Ollama, llama.cpp, hosted) | Low (6–24 h) | **GO**, as Mimir metrics and a Grafana panel. A script is optional. |
| 3 | **C3b Routing / prompt tuning** | Highest ceiling (model choice is a 20× price lever) | High (40–140 h, mostly the eval suite) | **DEFER** until C3a shows model differences and a task suite exists. Hand-written fallback rules are fine meanwhile. |
| 4 | **C2 Skill mining + proposals** | Low-medium. Evidence says self-generated skills don't help; SWE domain gains least | High (40–150 h) | **DEFER; no-go as automatic miner.** Revisit after ≥200 sessions if the C1 report shows a repeated sequence in ≥3 contexts. Hand-write skills meanwhile. |
| 5 | **C4 Preference inference** | Very low at N=1 user | Medium (20–80 h) | **NO-GO.** Use `AGENTS.md`. |

### D.2 Minimum data before each component's output is trustworthy

| Component | Minimum data [E] | Why | Gate to advance |
|---|---|---|---|
| Stage 1 logging | n/a | | ≥95% of LLM calls have full usage incl. cache fields; ≥99% of tool calls have outcome class, bytes, duration, normalized arg hash; one automated test replays a fixture session through the schema; secrets-redaction test passes. |
| C1 report | **≥30 sessions, ≥300 LLM calls, ≥3 distinct task types, ≥5 sessions with >10 turns** | Descriptive stats are stable enough to spot the top 1–2 waste categories. No inference needed. | Report names a waste category that is ≥10% of billed-token-equivalent cost **or** documents that none exists (a valid, useful result). Cache-hit ratio on repeat-prefix calls ≥70%, or you have found why not. |
| C3a stats | **≥20 sessions (~280 tool calls) per model** for rates to ±3 points at ~7% | CI width formula in §A.3 | Per-model dashboard shows malformed-call, loop and max-iteration rates with CIs. Two models differ by more than their CIs, or you've recorded that they don't. |
| C3b routing | **≥150 sessions *or* a fixed suite of ≥20 tasks × ≥3 repeats × models** | Success-rate comparisons need ~94–376 per arm (§A.3); a suite with CV 0.3 gets token comparisons at ~62 | A routing rule beats the single-model baseline on a **held-out** task set: cost −X% with success non-inferior (paired, McNemar or similar). |
| C2 skills | **≥200 sessions on recurring task types; a sequence with support ≥5 sessions across ≥3 contexts** | Mining below this finds coincidence | ≥2 approved skills, each evaluated on ≥60 paired suite runs: tokens down (CI excludes 0), success not worse on the suite *and* on old tasks (GRASP-style regression check). |
| C4 preferences | **≥500 sessions or ≥100 labeled correction events** | Rare events; classification noise | Do not build. |

### D.3 Cheapest experiment to validate or kill the idea in one week

**Use data you already have.** [M] Your machine holds 87 Claude Code transcript files (141 MB, 8 projects, oldest 2026-08-28) under `~/.claude/projects`. They contain tool calls, results, and usage with cache fields, for a far more mature harness. They are not your harness, but they tell you whether *the signals exist in your own behavior* before you build anything.

| Day | Task | Output |
|---|---|---|
| 1 | Parse the JSONL transcripts into a DuckDB table: session, turn, tool name, normalized args, result bytes, usage fields. | Table, ~2 h |
| 2 | Compute: (a) share of input tokens that are tool results; (b) re-read rate (same path read ≥2× in a session, file unchanged); (c) top-10 oversized tool outputs; (d) cache-hit ratio. | C1 mock report, ~2 h |
| 3 | Mine tool-call 3-grams and 4-grams with normalized args; count support across sessions and across the 8 projects. | Candidate patterns for C2, ~1 h |
| 4 | Count your corrections: user messages that start with "no", "don't", "stop", "revert", or follow a rejected tool call. | C4 base rate, ~1 h |
| 5 | Decide against the criteria below and write one page. | Decision |

**Kill / proceed criteria (set before you look):**

- **C1:** if tool-result bloat plus re-reads are **<10%** of input tokens, the report is not worth more than a dashboard panel. If ≥10%, build the report and fix at the source (output caps, cached repo map).
- **C2:** if no 3-gram/4-gram has support **≥5 sessions in ≥3 projects**, drop C2 permanently. If one does, write that skill **by hand** and see if it changes anything, before building a miner.
- **C4:** if corrections are **<1 per 10 sessions**, drop C4 permanently.
- **C3:** not testable on this data (single model family). Plan it from the harness's own data later.

Caveat: Claude Code sessions are not the same population as harness sessions. A positive result is encouraging, not proof. A negative result is stronger evidence, because a mature harness's users produce *more* structure than your harness will early on.

I did **not** run this experiment. I only counted the files to size it.

### D.4 Where your settled design ideas should change

| Your idea | Verdict | Change |
|---|---|---|
| Heavy lifting in plain code, LLM sees summaries | **Keep, and push further** | For C1 and C3a the LLM has no job. The analyzer is a report, not an agent. |
| Proposals versioned and human-approved | **Keep** | Add: skill bodies are human-written; LLM output is limited to "pattern found" pointers. |
| Analyze metadata, not raw outputs | **Right but insufficient** | Strings inside metadata (paths, argv, error messages) are also untrusted. Whitelist enums, counts and hashes. |
| Track quality alongside tokens | **Keep, but you need a quality signal** | Tests-pass exists only for some tasks and can be gamed. Add a one-keypress outcome label at session end and a fixed suite with checks the agent can't edit. |
| Closed loop: measure before and after | **Keep, with the sample sizes in §A.3** | Live traffic at N=10–100/month can't resolve effects under ~30% on tokens or ~20 points on success. |
| Replay past sessions to validate | **Change** | Split into (1) *counterfactual accounting*: "how many tokens would this rule have removed from the recorded context," which is deterministic, free and biased; and (2) *re-execution on a task suite*, which is noisy and costs ~$30–$72 per proposal on a hosted model (free but slow on local). Neither is "replay." Snapshot `git_head` and dirty diff at session start or neither works. |
| Source data: JSONL/SQLite, optionally Loki/Tempo/Mimir | **Change** | SQLite is the source of truth (joins, exact counts, no sampling). LGTM is for dashboards. Store `trace_id` in SQLite to link them. |
| Background scheduled sub-agent | **Change** | A cron'd script writing a markdown/HTML report. Add an LLM only at the stage-3 gate (§D.2), and only if the C1 report shows something worth proposing. |
| Staged build (1 logging, 2 report, 3 LLM skills, 4 routing) | **Keep stages 1–2; treat 3–4 as optional and gated** | Swap 3 and 4's order in value if you run multiple models: routing evidence (C3) precedes skills. |

### D.5 Sequencing against the real build order

The analyzer's prerequisites are, in order: (1) make `llm/` compile and finish the OpenAI-compatible client with `usage` captured; (2) orchestrator loop; (3) `write_file`/`run_shell` + permission gate; (4) SQLite session store; (5) telemetry with cache-token attributes; (6) ≥30 real sessions. **Do not start any analyzer work until step 5 exists.** Stage 1 of the analyzer is simply "make steps 4 and 5 emit the extra fields in §A.1," which costs little if designed in now and is painful to retrofit.

---

## E. Open questions for you

1. **Primary backend:** hosted API, or local Ollama/llama.cpp? If local, dollar ROI is ~$0 everywhere, and the framing should shift to latency, context fit and learning. It would also change the cost of re-execution validation from $30–$72 to free-but-slow.
2. **Expected volume:** how many sessions per month will you realistically run on the harness, versus on Claude Code or Codex for real work? A guess of 10 and a guess of 150 lead to different gates.
3. **What do you value an hour at, and how much of the build is "the goal"?** My learning-credit percentages (a8) are judgment calls. Change them and the break-even moves a lot.
4. **Is the portfolio audience impressed by a self-improving loop, or by a clear, well-measured system with an honest negative result?** A well-instrumented harness plus a one-page "here is what I measured and what I decided not to build" is arguably the stronger artifact.
5. **Quality signal:** are you willing to add a one-keypress outcome label per session, and to maintain a small fixed task suite (10–20 tasks with checks the agent can't edit)? Without one, C2, C3b and any before/after claim are unmeasurable.
6. **Which models will you actually run?** If it is only one, C3 collapses to "monitor that model." If three or more, C3a becomes the most useful component.
7. **Data retention and privacy:** will sessions touch private repos or secrets, such that full tool outputs must not be stored at all (hash + size only)?
8. **Do you want the GUI (Tauri) to own the approval flow?** C4 and the proposal-approval step both need a UI surface. Right now the CLI is a one-shot menu.

---

## Appendix A. Editable ROI model

Save as `roi_model.py` and run with `python roi_model.py`. Everything marked `# EDIT` is an assumption from §B.2. Index 0/1/2 = low/central/high.

```python
H        = 12        # EDIT a9 amortization horizon (months)
RATE     = 25        # EDIT a10 $/hour (0, 25, 50)
LEARNING = True      # EDIT apply learning credit a8?
CW, SW   = 1, 1      # which column for cost (CW) and saving (SW): 0 low, 1 central, 2 high

# name: (build_h, maint_h_per_mo, saving_frac_of_spend, llm_$_per_mo, sessions_needed, learning_credit)
C = {
 "C1  token report":      ((8,16,30),  (0.5,1,2), (0.02,0.08,0.20), (0,0,0),  30,  0.7),
 "C2  skill mining":      ((40,75,150),(2,4,8),   (0.0,0.02,0.08),  (1,3,8),  200, 0.3),
 "C3a model stats":       ((6,12,24),  (1,2,4),   (0.0,0.03,0.10),  (0,0,0),  40,  0.7),
 "C3b routing/prompt":    ((40,70,140),(1,3,6),   (0.0,0.10,0.40),  (2,6,20), 150, 0.3),
 "C4  preferences":       ((20,40,80), (2,4,8),   (0.0,0.01,0.03),  (1,3,8),  500, 0.1),
}

def monthly_cost(k, w=CW):
    b, m, s, l, n, lc = C[k]
    return b[w] * (1 - (lc if LEARNING else 0)) * RATE / H + m[w] * RATE + l[w]

def breakeven_spend(k):
    s = C[k][2][SW]
    return monthly_cost(k) / s if s > 0 else float("inf")

def months_to_payback(k, S, N):
    b, m, s, l, need, lc = C[k]
    net = S * s[SW] - m[CW] * RATE - l[CW]
    if net <= 0: return None
    return need / N + b[CW] * (1 - (lc if LEARNING else 0)) * RATE / net

# session cost on Sonnet 5.5 (prices per MTok: in 2, cache read 0.10, 5m write 2.5, out 10)
def session_cost(turns=15, ctx0=8000, ctx1=40000, out=600, hit=0.9):
    avg_in = (ctx0 + ctx1) / 2 * turns
    return (avg_in * hit * 0.10 + ctx1 * 2.5 + avg_in * (1 - hit) * 2 + out * turns * 10) / 1e6

if __name__ == "__main__":
    print("session $%.2f" % session_cost())
    for k in C:
        be = breakeven_spend(k)
        print(f"{k:22s} monthly cost ${monthly_cost(k):7.1f}   break-even spend ${be:,.0f}/mo")
    Ss = [10, 30, 100, 300, 1000, 3000]
    for k in C:
        print("\n" + k, "(rows N sessions/mo; cols S $/mo)", Ss)
        for N in (10, 30, 100, 300):
            row = []
            for S in Ss:
                p = months_to_payback(k, S, N)
                row.append("never" if p is None or p > 60 else f"{p:.1f}")
            print(N, row)
```

---

## Appendix B. Sources

Primary documentation (retrieved 2026-10-09):

1. [Anthropic: Prompt caching](https://platform.claude.com/docs/en/build-with-claude/prompt-caching)
2. [OpenTelemetry GenAI semantic conventions](https://opentelemetry.io/docs/specs/semconv/gen-ai/gen-ai-spans/) (page now redirects to the semantic-conventions-genai repo; status Development/Experimental)
3. [Anthropic: Pricing](https://platform.claude.com/docs/en/about-claude/pricing)
4. [OpenAI: Prompt caching](https://developers.openai.com/api/docs/guides/prompt-caching)
5. [llama.cpp discussion: KV cache reuse with llama-server](https://github.com/ggml-org/llama.cpp/discussions/13606) and a search-result summary on Ollama `keep_alive` (**secondary**)
6. [LiteLLM: Prompt caching](https://docs.litellm.ai/docs/completion/prompt_caching)
7. [Anthropic: Context editing](https://platform.claude.com/docs/en/build-with-claude/context-editing)
8. [Claude Code: Skills](https://code.claude.com/docs/en/skills)
9. [Claude Code: How Claude remembers your project](https://code.claude.com/docs/en/memory) (I read only the opening; the 200-line / 25KB figure comes from its comparison table)
10. [Aider: Advanced model settings](https://aider.chat/docs/config/adv-model-settings.html)
11. [Aider: Prompt caching](https://aider.chat/docs/usage/caching.html)

Research papers and analyses:

12. [SkillsBench](https://arxiv.org/html/2602.12670v1)
13. Memory poisoning: [MINJA](https://arxiv.org/html/2503.03704v5) and [Christian Schneider's overview](https://christian-schneider.net/blog/persistent-memory-poisoning-in-ai-agents/) (OWASP ASI06 reference is from search summaries; **secondary**)
14. [SpecBench: Measuring Reward Hacking in Long-Horizon Coding Agents](https://arxiv.org/pdf/2605.21384) (title and abstract-level only; I did not read the paper)
15. [LLM Agents Can Easily Tamper With Their Own Traces](https://arxiv.org/pdf/2609.30266)
16. [Rethinking Self-Evolving Agent Skills: Feedback Dynamics over Multiple Rounds](https://arxiv.org/html/2608.02636)
17. [GRASP: Gated Regression-Aware Skill Proposer](https://arxiv.org/pdf/2605.29668) (the fetch returned only a metadata-level summary)
18. [Agentic Context Engineering](https://arxiv.org/abs/2510.04618)
19. [Agent Workflow Memory](https://arxiv.org/pdf/2409.07429) and [Voyager](https://ar5iv.labs.arxiv.org/html/2305.16291)

Secondary / not independently verified (used only where marked **secondary**): community write-ups on [Claude Code compaction](https://claudefa.st/blog/guide/mechanics/what-survives-compaction), [OpenCode compaction issues](https://github.com/anomalyco/opencode/issues/18037), [Cursor memory](https://www.past.dev/blog/cursor-memory), [Codex CLI system prompts compared](https://codex.danielvaughan.com/2026/04/19/system-prompts-compared-codex-gemini-claude-code/), and [Cline Memory Bank](https://docs.cline.bot/best-practices/memory-bank).

## Appendix C. Limits of this analysis

- No code was written or run in the repo. `cargo check` was run once to confirm the lib does not compile. The ROI script was run in a scratch directory, and its outputs are pasted into §B.
- The spec file was read as text extracted from the HTML. Diagrams were not inspected.
- All effort, saving, volume and learning-credit numbers are my estimates. The sample-size figures use standard formulas with stated variance assumptions, not data from your system. The one-week experiment exists to replace the largest of these with measurements.
- I did not verify per-model tuning practices in Cursor, Cline or OpenCode beyond the sources above, and I did not read the full text of the GRASP, SpecBench or Agent Workflow Memory papers.
