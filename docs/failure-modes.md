# Failure-mode taxonomy for `lightning` sessions

Date: 2026-10-09. Status: draft for review. Definitions only, no ROI ranking yet and no implementation.
Builds on [analyzer-agent-analysis.md](analyzer-agent-analysis.md). Labels: **[M]** measured from repo/spec, **[E]** my estimate or prior.

## 1. Design stance

The goal is a harness that adapts to three things: the **user** (preferences), the **repo** (codebase nuances), and the **device** (local model speed, memory, OS). Three rules follow from your constraint that analysis must not itself burn tokens.

1. **Detect inline, not retrospectively.** The orchestrator already sees every call and result. Each failure mode below has a detector that is a counter or comparison on data already in the loop (a hash, a size, a timestamp). Cost: zero LLM tokens, microseconds of CPU.
2. **Prefer fixing at the source over learning.** Many modes disappear if the tool or prompt is designed well (e.g. a `read_file` with line ranges removes most "read the whole file" waste). Learning is for what static design cannot know: this user's habits, this repo's commands, this machine's limits.
3. **One row per session, not a transcript.** At session end, write a *fingerprint*: about 30 integers and enums (counts per failure mode, token totals, cache ratio, outcome label). Everything adaptive reads fingerprints. Raw events stay in SQLite for debugging, and no analysis step reads them by default. The LLM, if used at all, sees only fingerprints that tripped a threshold, and only enum labels and counts, never raw strings (injection hygiene from the earlier report).

**Detectability classes** used below:

- **D0**: computable inline from the current call/result/context (free).
- **D1**: needs a small cross-session comparison (a SQL query over fingerprints, still no LLM).
- **D2**: needs a human label or LLM judgment. Treat as proxy-only until proven.

**Adaptation target**: **U** = user profile, **R** = repo profile, **D** = device profile, **S** = static harness fix (no learning needed).

---

## 2. Failure modes

### A. Misunderstanding the user

These are the costliest per incident (they invalidate the work) but the hardest to detect. Almost none are D0. Their proxies are user reactions.

| ID | Failure mode | What it looks like | Signal | Class | Fix locus |
|---|---|---|---|---|---|
| U1 | Wrong target | Agent edits or investigates the wrong file, feature or layer | User message right after an agent action that negates or redirects it; edit reverted (pre/post-image hash); user interrupt | D2 (reverts and interrupts are D0 proxies) | U/R |
| U2 | Acted on an ambiguous request | Large write or shell action as the first move on an underspecified task, then a correction | Write/shell with no prior read or question; correction within next 1–2 turns | D1 | U |
| U3 | Over-asking | Clarifying questions where a sensible default existed | Question count per session; user replies like "whatever / you decide" | D1 | U |
| U4 | Scope creep | Files changed that the request didn't imply; extra refactors | Files written outside the paths named or implied by the task; diff size vs. median for similar tasks | D1 | U |
| U5 | False completion / stopped early | Final answer without running the verification that exists (tests, build) | `Completed` outcome with zero test/build commands in a repo that has a known test command | D0 (given R profile) | R |
| U6 | Repeated preference violation | Same correction needed again in a later turn or session | Normalized correction (or same denied action) recurs ≥2 times | D1 | U |
| U7 | Goal lost after compaction or truncation | Agent re-asks, redoes, or drifts once old turns are dropped | Context assembly invariant: original task and latest user message not present; re-read of task-relevant files immediately after compaction | D0 | S |

Note: U1, U2 and U6 are the only modes where "infer user preferences" truly applies. All three depend on a user-reaction signal that the planned schema does not yet record (user follow-ups linked to the preceding action, write pre-images).

### B. Unnecessary or wasteful tool calls

| ID | Failure mode | What it looks like | Signal | Class | Fix locus |
|---|---|---|---|---|---|
| T1 | Redundant re-read | Same file read again though unchanged and its earlier content is still in the window | `(path, content_hash)` already read this session, no intervening write, prior result still in context | D0 | S |
| T2 | Redundant list/search | Same `list_dir` or `grep` args repeated, or a strict subset of an earlier call | Normalized `(tool, args)` seen before with no intervening write | D0 | S |
| T3 | Whole-file read where a range or search would do | Reads a 2,000-line file to find one function | Lines/bytes returned vs. a threshold; later action touches a small span (D2) | D0 for size, D2 for waste | S (add ranges) |
| T4 | Unbounded tool output | `grep` with thousands of matches; recursive listing; command dumping logs | Result tokens > N; truncation hit; single result > X% of window | D0 | S (output caps) |
| T5 | Retry without change | Identical call repeated after an error | Same `(tool, args_hash)` immediately after an error result | D0 | S |
| T6 | Flailing after errors | ≥k consecutive failed calls without new information | Consecutive-error streak length | D0 | S/R |
| T7 | Serial calls that could be one turn | Several independent read-only calls issued one per LLM round trip | Run of consecutive read-only calls with no data dependency (arg doesn't reference prior result). Each costs a full model call. **[M]** The current client keeps only the first tool call, so parallel calls are impossible by construction | D0 | S |
| T8 | Verify-by-rereading | `write_file` followed by `read_file` of the same path | Read of path P directly after write of P. Cure: write result returns a short diff or checksum | D0 | S |
| T9 | Wrong tool for the job | `run_shell` with `cat`/`grep`/`find`/`ls` when dedicated tools exist | argv0 in a small set | D0 | S |
| T10 | Repeated build/test with no change | Same test/build command rerun with no intervening write; or full output dumped every time | Identical command hash, no write between; output tokens per run | D0 | S/R |
| T11 | Ignored-path reads | Reading `target/`, `node_modules/`, `.git/`, lock files, build output | Path prefix match on a deny-for-read set. **[M]** this repo has a large `target/` directory in the working tree | D0 | S/R |
| T12 | Dead-end exploration | Calls whose results never influence later actions | No cheap deterministic test exists | D2 | not worth chasing |

### C. Inefficient use of context

| ID | Failure mode | What it looks like | Signal | Class | Fix locus |
|---|---|---|---|---|---|
| C1 | Stale tool results retained | Old large results stay in history long after they matter | Share of window made of tool results older than k turns and not referenced since | D0 | S (prune old results first) |
| C2 | Cache-busting prefix | Prefix changes between calls (timestamp in system prompt, reordered or changed tool schemas, edited history) so the provider cache misses | Hash of `(tools, system, history[..n])` differs from previous call's prefix; `cache_read_tokens / input_tokens` below a threshold on consecutive calls | D0 | S |
| C3 | Cold-cache gaps | Idle > cache TTL (Anthropic 5 min default; local model unloaded by Ollama after 5 min) forces a full re-prefill | Time since previous call vs. TTL; `cache_creation` spike or prefill latency spike | D0 | D (keep-alive) / S (1h TTL when worthwhile) |
| C4 | Fixed-overhead bloat | System prompt + tool schemas + instruction files are a large share of every call | `(system+tools+instructions) / input_tokens`, per call | D0 | S |
| C5 | Duplicate content | The same file or snippet present twice in the window | Content-hash collision across blocks | D0 | S |
| C6 | Compaction thrash or loss | Compaction fires repeatedly, or the agent re-reads files right after compaction | Compaction count; re-read rate within 3 turns after compaction | D0 | S (tune thresholds; keep file index) |
| C7 | Output verbosity | Long narration, repeated summaries, or unneeded reasoning before obvious calls | Output tokens per turn vs. session median and vs. result size | D0 | S/U (some users want terse) |
| C8 | Context sized wrong for the device | Window larger than the machine handles (swap, slow prefill) or too small (constant truncation) | Prefill latency per 1k tokens rising with context; truncation events; OOM/restart | D0 | D |
| C9 | Cost runaway | Session far exceeds expected tokens for its type | Cumulative tokens vs. a per-session budget | D0 | S |

### D. Loops and control flow

| ID | Failure mode | What it looks like | Signal | Class | Fix locus |
|---|---|---|---|---|---|
| L1 | No-progress loop | Repeating similar actions, state unchanged | Hash of `(last k calls, workspace state)` repeats | D0 | S |
| L2 | Oscillation | Edit A, revert A, edit A again | Same file's content hash returns to an earlier value | D0 | S |
| L3 | Max-iteration hit | Cap reached without a final answer | Outcome enum | D0 | S (diagnose which of L1/T5/T6/L4 caused it) |
| L4 | Exploration without action | Many turns of reads/searches with no write or test | Turns since last write/test above a threshold | D0 | S/R |

### E. Model–tool protocol errors

The spec warns that small local models are unreliable at tool-calling, so expect these to cluster on local models.

| ID | Failure mode | What it looks like | Signal | Class | Fix locus |
|---|---|---|---|---|---|
| P1 | Malformed JSON arguments | Unparseable arguments string | Parse failure at the LLM client (separate from schema errors) | D0 | S/D (model-specific prompt) |
| P2 | Schema-invalid arguments | Missing/mistyped field, or wrong field name. **[M]** Spec says `path`, code says `file_path`/`dir_path`: this mismatch type is a likely trap for models primed on other harnesses | Schema validation failure with field name | D0 | S (fix names, improve descriptions) |
| P3 | Unknown/hallucinated tool | Calls a tool that doesn't exist | Unknown-name error. **[M]** `ToolError::InvalidArguments` currently conflates this with P2 | D0 | S |
| P4 | Path errors | Absolute paths, paths outside the workspace, nonexistent paths | `NotFound` / `PathOutsideProject` counts and the path shape | D0 | S |
| P5 | Tool call emitted as text | Model writes the call in prose or markdown instead of structured output | Text response matching tool-call patterns (D0), or response with no call and no completion | D0 | D (model-specific) |
| P6 | Truncated or empty response | `finish_reason` is length or the response is empty | Finish reason (**[M]** the client does not capture it today) | D0 | S/D |
| P7 | Ignores tool result | Answer contradicts or disregards the result | none cheap | D2 | skip |

### F. Permission and gate friction

| ID | Failure mode | What it looks like | Signal | Class | Fix locus |
|---|---|---|---|---|---|
| G1 | Repeated policy denial | Agent keeps proposing a disallowed command | Same denied `(tool, argv0, subcommand)` repeated | D0 | S (put policy in the prompt/tool description) |
| G2 | Approval fatigue | User approves the same safe pattern again and again | Same normalized action approved ≥k times | D1 | U (propose an allow rule; user approves it) |
| G3 | User rejection | User denies an action | Count by normalized action; feeds U6 | D0 once `AskUser` exists. **[M]** spec marks it v2 | U |

G2 is the cheapest real *preference learning* in this document: deterministic, tiny, and its output is a single allow-rule the user confirms.

### H. Repo and device nuances

| ID | Failure mode | What it looks like | Signal | Class | Fix locus |
|---|---|---|---|---|---|
| E1 | Guessing the build/test command | First `run_shell` fails (command not found, wrong tool), a later one succeeds | Non-zero or not-found shell result followed by a different successful command on the same intent | D0 | R (detect from `Cargo.toml`, `package.json`, `Makefile`; cache it) |
| E2 | Rediscovering the repo each session | Same opening sequence of list/read calls in every session | First-N-calls sequence hash equal across ≥k sessions of the same repo | D1 | R (cached repo map / profile) |
| E3 | Slow tool or timeout | Shell or grep exceeds time budget | Tool duration percentiles; timeout count | D0 | R/D |
| E4 | Local model throughput limit | Tokens/s too low, prefill dominates, model reloads | Time to first token, tokens/s, load events per session | D0 | D |
| E5 | Windows-specific breakage | Path separators, `\\?\` canonical paths, CRLF in diffs/edits, `ls` vs `dir`, shell differences | Error class tagged by OS-sensitive causes. **[M]** you're on Windows 11 and `Workspace::resolve` relies on `fs::canonicalize`, which yields extended-length paths on Windows | D0 | D/S |
| E6 | Flaky or pre-existing test failure blamed on the change | Agent "fixes" a failure the change did not cause | Same test fails at baseline (needs one baseline run) | D1 | R |

---

## 3. Which modes carry the adaptive part of the goal

Most modes are best removed by static design (S). The ones that need *learning* map to your three axes:

| Axis | Modes that genuinely need adaptation | Mechanism (no LLM tokens) |
|---|---|---|
| **User** | U2, U3, U4, U6, G2, G3, C7 | Per-user counters: verbosity preference, ask-vs-act tolerance, approved patterns. Output is a short, human-readable profile of settings and allow-rules, not free-text memory. |
| **Repo** | E1, E2, E6, U5, T10, T11, L4 | Auto-detected, cached facts: build/test commands, ignore paths, top-level layout, known-flaky tests. Invalidated by file hash. |
| **Device** | C3, C8, E3, E4, E5, P1, P5 | Measured limits: usable context size, keep-alive setting, tool timeouts, per-model prompt/format choice. |

The remaining modes (T1–T9, C1, C2, C4–C6, C9, L1–L3, P2–P4, P6, G1) are generic efficiency bugs. They are detected by the same inline counters but fixed once in the harness, not per user.

## 4. Minimum signals the loop must record

All D0/D1 detectors above reduce to this set. Items marked **new** are not in the spec's planned `events` table or span attributes.

| Signal | Used by | Status |
|---|---|---|
| Per-call `input/output/cache_read/cache_write` tokens, finish reason, latency, time-to-first-token | C2–C4, C7–C9, E4, P6 | **new** (cache fields, finish reason, TTFT) |
| Hash of prefix segments (tools, system, history) per call | C2, C5 | **new** |
| Per tool call: normalized args hash, result bytes, result tokens, duration, error class (separate: bad JSON / schema / unknown tool / path / runtime / denied / timeout) | T1–T11, P1–P4, G1, E3 | **new** (today only `line_count`/`item_count`; error classes conflated) |
| Content hash + mtime of files read and written | T1, T8, L2, U1 | **new** |
| Context composition per call: tokens by source (system, tools, instructions, user, assistant, tool results by age) | C1, C4 | **new** |
| Compaction/truncation events | C6, U7 | **new** |
| Permission decision and (later) user response | G1–G3 | partial (decision exists in spec; user response is v2) |
| User message linked to the preceding agent action | U1, U2, U6 | **new** |
| Session outcome enum plus one-keypress user label (ok / partly / wrong) | U1, U5, L3 | partial (outcome enum yes, user label **new**) |
| Repo/device identifiers: git head, OS, model id + backend, context size setting | E-series | **new** |

## 5. Open questions for you

1. **Which of the 3 axes matters most to you first?** (user preferences, repo, device). They have different costs: device and repo profiles are deterministic and cheap, user-preference learning is the noisy one.
2. **Do you accept a one-keypress session label** (ok / partly / wrong)? Without it, the whole misunderstanding family (U1–U7) has only indirect proxies.
3. **Any modes you'd add from experience with other harnesses?** The list is derived from the spec, the code, and the literature, not from your own sessions.
4. **Severity:** I haven't weighted modes by cost. The next step is to assign each a rough cost-per-incident and prior frequency, then rank by ROI and feasibility together, as you proposed.
