# Model Ledger — Project Spec (working title)

> A harness-agnostic, local-first tool that reads the session logs your coding agents already write, works out **which model is worth using for which kind of task**, and turns that into reviewable, reversible configuration changes.

Status: draft v0.1 · Audience of this document: the builder (and Claude Code, as the implementer)

---

## 1. One-paragraph summary

People who use Claude Code, Codex and similar harnesses switch models constantly: Opus for hard problems, Sonnet for everyday work, Haiku for subagents, sometimes mid-session. Today they have good tools for *how much they spent* and decent built-in tools for *what instructions they repeat*, but nothing that answers **"for the work I actually do, which model gave me the best result per dollar, where does each model fail, and what should I change?"** Model Ledger ingests the local session transcripts, normalizes them into one schema, computes per-model, per-task-type cost and reliability metrics (with honest confidence), and proposes small diffs (default model, subagent model, model-scoped rules) that the user approves. It is delivered as a CLI plus a skill, an MCP server, and an optional passive weekly report, packaged as a plugin for existing harnesses.

## 2. Why we are doing this

### 2.1 The situation
- Modern harnesses already write full local transcripts as JSONL (Claude Code under `~/.claude/projects/**`, Codex rollouts under `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl`). The raw data exists on every user's machine.
- Model choice is now a per-task, per-subagent decision, and users make it by feel.
- Cost matters at least as much as capability. Wasted runs and retries turn directly into spend.

### 2.2 The gap
Existing tools answer "how much" and "what do I repeat." None that we found answers:
1. Cost **per successful outcome** by model (not just cost by model).
2. Which **task types** each model is good or bad at *for this user*.
3. Whether a **mid-session model switch** hurt (lost cache, re-read context, more corrections).
4. Which **rules a specific model ignores**, so rules can be scoped to the model that needs them.
5. Whether an adopted suggestion **actually worked** (before/after measurement).

> Verification note: "none that we found" is based on a limited web search in Oct 2026. Re-check before positioning publicly. See §14.

### 2.3 Why a separate tool and not a feature of the Rust harness
See §4. Short version: adoption. Users will not abandon Claude Code or Codex for a new harness, but they will install a plugin.

## 3. Goals and non-goals

### Goals
- G1. Ingest transcripts from at least Claude Code and Codex into one normalized schema.
- G2. Produce per-model, per-task-type cost and reliability reports with explicit confidence and an "insufficient data" state.
- G3. Surface model-switch effects.
- G4. Generate **reviewable diffs** (never silent changes) for model defaults, subagent models, and model-scoped rules.
- G5. Measure the effect of adopted suggestions (closed loop).
- G6. Be local-first, private by default, and cheap to run (no LLM required for the core report).
- G7. Ship as: CLI, skill, MCP server, passive weekly report, and a Claude Code plugin / Codex install path.

### Non-goals (what we do NOT compete on)
- N1. **Plain usage and cost dashboards.** ccusage-style daily/monthly/session cost reports and live burn-rate monitors are solved and well adopted. We consume the same data but do not rebuild their UX. Our cost numbers exist to feed model comparison.
- N2. **Generic friction reports and CLAUDE.md rule suggestions.** Claude Code's built-in `/insights` already covers repeated-instruction mining for its own users. We only generate rules that are *model-scoped and evidence-backed per model*.
- N3. **General skill mining** from repeated tool sequences. Community tools already do this across many agents. Skill proposals are out of scope for v1 (possible later, as a consumer of our data).
- N4. **Memory systems** (auto memory, consolidation, Codex memories, third-party memory plugins).
- N5. **Hosted APM / OpenTelemetry pipelines.** We may export our normalized events in OTel-compatible JSONL, but we are not building a tracing backend.
- N6. **Team dashboards and gallery/curation products.**
- N7. **A new agent harness.** (The separate Rust harness is a different project; see §4.)
- N8. **Automatic application of changes.** Everything is a proposal.

## 4. Relationship to the Rust agentic harness

| | Rust harness (separate project) | Model Ledger (this project) |
|---|---|---|
| What it is | A new coding agent harness | An analyzer + advisor that plugs into existing harnesses |
| Who runs the agent | The harness itself | Claude Code, Codex, etc. (not us) |
| Data source | Its own native event log | Other harnesses' transcripts (plus the Rust harness as one more adapter) |
| Adoption path | Users must switch tools | Users keep their tools and add a plugin |
| Primary goal | Learn Rust + LGTM; auditable agent | Useful, adoptable tool that fills a real gap |
| Dependency | None on this project | None required; harness can later emit our schema natively |

Design rule: the **normalized event schema (§7)** is the shared contract. The Rust harness can emit it natively later, becoming a first-class adapter, but neither project blocks the other.

## 5. Target audience

### Primary
- **Heavy individual users of Claude Code and/or Codex who switch models** (Opus/Sonnet/Haiku, or across vendors) and want evidence for routing decisions. They already read `ccusage` and may have tried `/insights`.
- **Users who configure subagents** with per-subagent models and want to know which assignments are wasteful.

### Secondary
- **Local-model users** (Ollama, llama.cpp via a harness) where tool-call reliability varies a lot by model and quantization, and where "cost" is time and VRAM rather than dollars.
- **Small teams** who want to share *aggregated, redacted* findings (post-v1).

### Not the audience
- Casual users with a single model and a few sessions per week (not enough data; built-in tools are enough).
- Enterprises needing governance/compliance dashboards.

### Jobs to be done
1. "Tell me whether I'm overpaying for Opus on tasks where Sonnet does fine."
2. "Tell me where each model fails me, so I stop finding out the hard way."
3. "Give me the exact config change, as a diff, and tell me later if it helped."

## 6. Landscape (as of Oct 2026; verify before publishing)

| Existing tool / feature | What it does | Overlap with us | Our stance |
|---|---|---|---|
| `ccusage` (~16k stars per third-party trackers; multi-harness) | Token and cost reports from local logs, by date/session/model; has an MCP server | Data ingestion, cost by model | Reuse ideas/format knowledge; don't compete on dashboards. Possibly use as an optional data source |
| Claude-Code-Usage-Monitor | Live burn-rate and limit prediction | Minimal | Ignore |
| Claude Code `/usage`, `/context`, `/cost` | Built-in session spend and context fill | Minimal | Ignore |
| Claude Code `/insights` | 30-day report, friction analysis, CLAUDE.md suggestions, skill suggestions | Rule suggestions | Differentiate: model-scoped, measured |
| Auto-skill-style tools (e.g. auto-skill, self-learning-skills) | Detect repeated workflows, generate SKILL.md | Skill mining | Out of scope for v1 |
| Claude Code auto memory + Auto Dream; Codex Memories; memory plugins | Persist and consolidate project knowledge | None | Out of scope |
| claude-otel and similar | Export session traces to OTLP/JSONL | Export format | Optional compatibility only |
| skillscope, ClaudeLens | Skill-invocation analytics; team session gallery | Parsing, analytics | Not competing; may borrow parsing lessons |

What we found no evidence of (hypothesis to re-verify): cost per successful outcome by model and task type; model-switch cost analysis; model-scoped rule suggestions; before/after measurement of adopted suggestions.

## 7. Core concepts and normalized schema

All adapters emit the same records. JSONL is the canonical interchange; SQLite is the working store.

### 7.1 Entities
- **Session**: one harness conversation (source harness, version, project/cwd, start/end).
- **Turn**: one model response. Fields: model id, input/output/cache-read/cache-write tokens, latency if available, stop reason, is_subagent, parent session.
- **ToolCall**: tool name, argument hash (not raw args), result status (ok/error), error class, output size, duration if available.
- **UserSignal**: a user message classified as one of: new-task, correction, clarification, approval, interruption/abort. Stored as classification + hash, not raw text.
- **PermissionEvent**: asked / allowed / denied.
- **ModelSwitch**: derived event when consecutive turns in a session use different models.
- **OutcomeSignal**: derived per task segment (see §8.4).
- **Suggestion** and **Adoption**: proposals and the user's decisions, with timestamps and config hash before/after.

### 7.2 Schema rules
- Every line has `schema_version`. Additive changes only.
- Store hashes and metadata, **not raw prompts, file contents, or tool output**, by default. A `--keep-raw` flag may exist for local debugging only.
- Field names follow OpenTelemetry conventions where natural (timestamps, ids, flat attribute maps) so users can pipe events into other tooling.

## 8. Analysis

### 8.1 Principle
Counting and statistics are plain deterministic code. An LLM is optional and only sees **aggregated summaries**, never raw logs.

### 8.2 Metrics (per model, per task type, per repo; all with n and confidence)
- Tokens by type and estimated cost (pricing table is versioned data, editable by the user; local models use time/tokens only).
- Tool-call error rate and invalid-argument rate.
- Retry and no-progress loop rate (same tool + same args hash repeated, or edits reverted).
- Re-read rate (same file hash read multiple times) and oversized tool output count.
- Correction rate (user corrections per task segment) and abort rate.
- Permission denial rate.
- Cache hit ratio and cost impact around model switches.
- Turns and tokens to completion per task segment.

### 8.3 Task segmentation and task-type labeling
- **Segment** a session into tasks at new-task user signals or long idle gaps.
- **Task type** (v1 heuristics): debugging/fix, refactor, new feature, test writing, exploration/explanation, config/devops, docs. Based on tool usage mix (e.g. test commands, edit-heavy vs read-heavy), file types touched, and user-signal keywords matched locally.
- Optional LLM labeler (v1.1): given a *redacted summary* of a segment, return one label plus confidence. Always cached; never required.
- Labels carry confidence. Low-confidence segments are excluded from comparisons rather than guessed.

### 8.4 Outcome signals (the hard part)
Success cannot be observed directly. Use a composite of weak signals and report it as a *proxy*:
- Positive: tests/build command exits 0 near the end of a segment; user approval/continuation; no revert; segment ends without a correction.
- Negative: failing last test run, user correction right after completion, reverted or re-edited files, abort, repeated re-ask of the same request.
- Each segment gets `outcome ∈ {likely_success, likely_failure, unknown}` with the contributing signals recorded for auditability.
- Never present proxy outcomes as ground truth. Reports say "proxy success rate."

### 8.5 Selection bias (the credibility risk)
Users pick stronger models for harder tasks, so naive comparisons mislead. Mitigations:
- Stratify by task type, session size, and repo before comparing.
- Prefer **matched comparisons** (similar task type and size) and report confidence intervals.
- Minimum sample size per cell (default n ≥ 20 segments; configurable). Below it, output "insufficient data."
- Where a user has alternated models on similar work, call that out as the most trustworthy evidence.
- Include a "what would change this conclusion" note in every recommendation.

### 8.6 Model-switch analysis
For each ModelSwitch, compare the next N turns against a baseline: cache-read ratio drop, token spike, correction within K turns. Aggregate to "switches from A to B within a session tend to cost X." Treat the cache mechanics claim as an assumption to verify against current provider docs.

## 9. Suggestions (diff proposals)

### 9.1 Types (v1)
1. **Default model change** for a task type or project (e.g. "use Sonnet by default in repo X").
2. **Subagent model assignment** (e.g. "explore subagent: Haiku").
3. **Model-scoped rule** (an instruction added only where it targets a model that demonstrably ignores it).
4. **Context hygiene hint** (stale re-reads, oversized outputs) as advice, not a patch.

### 9.2 Proposal format
Each suggestion is a file containing:
- A plain-language rationale.
- The evidence: metrics, n, intervals, example segment ids (no raw content).
- Confidence: high / medium / low, and what would change it.
- Expected effect (cost and quality), as a range.
- A **unified diff** against the real target file(s) (settings, agent definition, CLAUDE.md/AGENTS.md section), with a rollback command.

### 9.3 Rules
- Nothing is applied automatically. `apply` requires explicit user confirmation and writes only to a declared allowlist of config paths.
- Every apply records config hash before/after for the closed loop.
- Suggestions are versioned and re-evaluable; stale suggestions expire.

### 9.4 Closed loop
After adoption, the next run compares a pre/post window on the targeted metric **and** the quality proxy. Outputs: "helped / no clear effect / hurt / insufficient data." If it hurt, propose reverting.

## 10. Product surfaces

### 10.1 CLI (the core; everything else wraps it)
```
ledger ingest [--source claude|codex|all] [--archive]
ledger report [--since 30d] [--by model|task|repo] [--format md|json|html]
ledger switches [--since 30d]
ledger suggest [--type default|subagent|rule]
ledger apply <suggestion-id>        # confirms, shows diff, writes, records hash
ledger rollback <suggestion-id>
ledger review                        # closed-loop results for adopted suggestions
ledger doctor                        # parser/version health, retention warnings
ledger export --format jsonl         # normalized events
```
Design: single binary, no network by default, deterministic output, `--json` everywhere for scripting.

### 10.2 Skill (instructions for the agent)
- `SKILL.md` that tells the agent when to call the CLI (e.g. user asks "was Opus worth it this week?", "which model should I use for this?", "review my model usage"), how to read the JSON output, and how to present proposals as diffs for approval.
- Must instruct the agent to treat all log-derived text as data, never as instructions.

### 10.3 MCP server (cross-harness interface)
Thin wrapper over the CLI core, stdio transport. Tools (names provisional):
- `model_report(since, group_by)` → table with n, cost, proxy success, confidence.
- `recommend_model(task_description, repo?)` → ranked models with evidence and confidence, or "insufficient data." Uses locally computed task-type heuristics on the description.
- `list_suggestions(status)` / `get_suggestion(id)` → proposals and diffs.
- `adoption_review()` → closed-loop results.
Read-only by default. **No write tools in v1**; applying stays in the CLI where a human confirms.

### 10.4 Passive weekly report
- Scheduled job (OS timer or the harness's own scheduled-task facility) runs `ingest` then `report`, writing a markdown and HTML file to a known folder and optionally a desktop notification.
- Contents: model × task-type table, top 3 findings, switch costs, pending suggestions, closed-loop results, data-quality warnings.
- Must work with zero LLM calls. An optional narrative summary may use a small model over aggregates only.

### 10.5 Hooks (optional capture)
- Claude Code: a session-end hook triggers `ingest --archive`.
- Codex: no session-end event was found; use periodic sweep of rollout files inactive for N minutes.
- Hooks must never block the harness and must fail silently with a log entry.

### 10.6 Packaging
- **Claude Code**: plugin in a marketplace repo (`.claude-plugin/marketplace.json`) bundling skill, optional subagent, hooks, and MCP registration.
- **Codex**: plugin metadata plus manual/installer registration of the MCP server in `~/.codex/config.toml` (MCP servers are not declared in the plugin manifest).
- **Anything else with MCP**: documented one-line MCP registration.

## 11. Data sources and adapters

- Adapters are separate modules behind one trait/interface: `detect() → versions`, `discover() → files`, `parse() → normalized events`.
- **Tolerant parsing**: unknown fields ignored, unknown event types counted and reported by `doctor`, never a crash.
- **Fixture corpus** per harness version (synthetic sessions, no real user data) with golden-output tests. Each format change adds a fixture.
- **Retention**: Claude Code deletes local transcripts after 30 days by default (`cleanupPeriodDays`), so `ingest --archive` copies/normalizes into our own store, and `doctor` warns the user and shows the setting to raise.
- **Subagent transcripts** are ingested and tagged `is_subagent` with the parent link.
- v1 adapters: Claude Code, Codex. v1.1: OpenCode and others on demand. Later: the Rust harness's native log.
- Formats are undocumented and may change. Treat every adapter as best-effort with a visible health status.

## 12. Privacy and security

- **Local-only by default.** No telemetry, no network calls in core paths. Any optional LLM use is opt-in and sends aggregates only.
- **Metadata over content.** Store hashes, counts, classifications. Do not persist raw prompts or tool outputs.
- **Redaction** pass on anything that could leave aggregates (secrets patterns, env-like strings, paths normalized).
- **Prompt injection.** Transcripts contain untrusted text. The analyzer never feeds raw transcript content to an LLM, and the skill instructs agents to treat tool output as data. Suggestions cannot contain free text copied from logs.
- **Write boundary.** `apply` can only modify an explicit allowlist of config files, shows a diff first, and records hashes. No shell execution from suggestion content.
- **Supply chain.** Pin dependencies, publish checksums, keep the binary small and auditable.

## 13. Technical approach

- **Language**: Rust for the core CLI (single static binary, fast JSONL parsing, fits the learning goals). A Python implementation is an acceptable fallback if iteration speed matters more early on, since the schema is the contract.
- **Storage**: SQLite for the working store, JSONL for export. Migrations versioned.
- **Statistics**: simple, explainable methods (stratified rates, bootstrap or Wilson intervals). Avoid opaque models.
- **Pricing data**: versioned, user-overridable table; `doctor` warns when it is stale.
- **MCP**: stdio server over the same library crate as the CLI (one core, three front ends).
- **Testing**: golden tests on fixtures; property tests for parsers (fuzz malformed lines); synthetic-session generator for statistical tests with known ground truth (e.g. a planted "Model B ignores rule X" effect must be detected, and a planted null must not produce a recommendation).
- **Performance budget**: ingest 10k sessions in minutes on a laptop; report in seconds from the store; idle footprint zero (no daemon in v1).

## 14. Validation plan (gate before building more than the CLI)

### 14.1 One-week feasibility experiment
1. Build only `ingest` + `report` for Claude Code transcripts, no LLM.
2. Run it on your own data. Label ~30–50 segments by task type and outcome by hand.
3. Compare models per task type. Check agreement between heuristic labels and your hand labels.
4. Run `/insights`, ccusage and an auto-skill tool on the same data and write down which questions they cannot answer.

### 14.2 Go / no-go criteria
- Task-type heuristics agree with hand labels ≥ ~80% on the labeled set.
- Outcome proxy agrees with hand-judged outcome ≥ ~75%.
- At least one cell shows a model difference that is large, stable across weeks, and survives stratification.
- At least one finding you did not already know and would act on.
- If models are indistinguishable for your work, or the proxy is too noisy: stop, or narrow to the cost/waste diagnostics only.

### 14.3 Re-verify before public positioning
- That no widely adopted tool already provides cost-per-successful-outcome by model/task type.
- Actual log field names for model id, tokens, cache tokens, tool results in current Claude Code and Codex versions.
- Prompt-cache behavior on model switches in the current provider docs.
- Current plugin/marketplace and MCP registration mechanics for each harness.

## 15. Roadmap

| Milestone | Scope | Gate to proceed |
|---|---|---|
| M0 | Schema, Claude Code adapter, `ingest`, `report` (cost, tool errors, re-reads) | Parsing health on your real data; §14.2 criteria |
| M1 | Task segmentation + labeling, outcome proxy, stratified model comparison, `doctor`, archive | Labeled-set agreement thresholds met |
| M2 | Codex adapter, `switches` analysis, passive weekly report | Cross-harness schema holds without hacks |
| M3 | `suggest` + `apply` + `rollback` with diffs; closed-loop `review` | Planted-effect tests pass; no false recommendations on null data |
| M4 | Skill + MCP server (read-only) + Claude Code plugin packaging + Codex install docs | Works end to end in a clean environment |
| M5 | Optional LLM labeler and narrative summary over aggregates; local-model metrics | Demonstrably better than heuristics on the labeled set |
| Later | Rust harness adapter, team aggregation, skill proposals | Real user demand |

## 16. Success metrics

- You use the weekly report and act on at least one suggestion per month.
- Adopted suggestions show a measurable cost reduction without a quality-proxy drop (closed loop).
- Parser breakage after harness updates is detected by `doctor` before it produces wrong numbers.
- External adoption (stars, installs, issues) only after the above; do not optimize for it first.

## 17. Risks and open questions

| Risk | Mitigation |
|---|---|
| Selection bias makes comparisons misleading | Stratification, matched comparisons, n thresholds, "insufficient data" |
| Outcome proxy is too noisy | Report as proxy; fall back to cost/waste diagnostics only |
| Log formats change without notice | Tolerant parsing, fixtures per version, `doctor` health checks |
| First-party tools absorb the feature | Stay narrow (model/task evidence + closed loop); data and schema stay useful regardless |
| Retention deletes data before analysis | Archive on hook/schedule; warn in `doctor` |
| Prompt injection via logs | Metadata-only analysis, no raw content to LLMs |
| Small sample size for single users | Clear thresholds; pooled/anonymized sharing is a post-v1 idea |
| Scope creep into dashboards/memory/skills | Non-goals in §3 are binding for v1 |

Open questions:
1. Is it better to implement `recommend_model` as heuristic-only in v1, or wait for the labeler?
2. Should the tool ever write to harness config directly, or only emit patch files the user applies?
3. How should local-model "cost" be defined (wall-clock, tokens, VRAM-seconds)?
4. Which outcome signals are reliably present in both Claude Code and Codex logs?
5. Do we need a stable public schema spec so other tools can adopt it?

## 18. Appendix

### A. Example weekly report (mock)
```
Model Ledger — week of 2026-10-05
Data: 142 segments, 3 repos, 2 harnesses. Parser health: OK.

Model × task type (proxy success, cost/segment, n)
                 debugging        refactor        test-writing
Opus             92% $0.84 (n=21) 88% $1.40 (n=14) insufficient (n=6)
Sonnet           89% $0.31 (n=48) 76% $0.62 (n=19) 91% $0.22 (n=24)

Findings
1. Debugging: Sonnet within 3 pts of Opus at ~37% of cost (medium confidence).
2. Refactor: Sonnet shows more corrections; difference persists after stratifying by size (medium).
3. 9 mid-session switches Sonnet→Opus cost an extra ~X cached-read tokens each (low confidence).

Pending suggestions: 2 · Closed-loop: "Sonnet default for test-writing" → helped.
```

### B. Example suggestion (mock)
```
id: sug-0007   type: default-model   confidence: medium
Rationale: For test-writing segments (n=24 vs n=6), Sonnet matches Opus proxy success at ~26% of the cost.
Would change if: Opus sample grows and shows a consistent >10pt success edge.
Expected effect: -$X to -$Y per week; quality proxy unchanged (range).
--- a/.claude/settings.json
+++ b/.claude/settings.json
@@ ... @@
-  "model": "opus"
+  "model": "sonnet"
Rollback: ledger rollback sug-0007
```

### C. Example MCP tool contract (conceptual)
```
recommend_model(task_description: string, repo?: string) -> {
  task_type: string, task_type_confidence: number,
  ranking: [{ model, proxy_success, cost_per_segment, n, confidence }],
  verdict: "recommend" | "insufficient_data",
  caveats: [string]
}
```

### D. Glossary
- **Segment**: one task within a session. **Proxy success**: composite weak-signal outcome estimate. **Stratify**: compare like with like (same task type/size). **Closed loop**: measuring whether an adopted change helped.
