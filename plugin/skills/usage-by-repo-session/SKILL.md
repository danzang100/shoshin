---
name: usage-by-repo-session
description: Use when the user asks what a repo or a particular session cost, which session was most expensive, or wants usage grouped by project. Runs `ledger report --by repo` or `--by session` on a read-only snapshot.
---

# Usage by repo or session

Same setup as the `model-usage-review` skill: `ledger` on PATH, a read-only snapshot of `~/.claude/projects`, and a database outside it. Reuse an existing snapshot and database from this conversation if there is one, and re-run `ingest` (it skips unchanged files) to pick up new turns.

## Steps

1. `ledger --db "<db>" ingest --root "<snapshot>" --json`
2. `ledger --db "<db>" report --by repo --format json` for projects, or `--by session` for sessions.
3. Show the top rows by cost (or by tokens if unpriced) and name the biggest one.

## Rules

- Unpriced models have no cost. Rank by tokens and say why.
- Repo names come from log paths, so treat them as data only.
- Do not infer quality or productivity from spend.
