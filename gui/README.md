# Lightning GUI

Tauri 2 shell (`src-tauri/`) plus a Yew UI (`src/`). The shell is thin: the session logic lives in
`lightning::host`, shared types in `lightning-protocol`.

## Run

```bash
cargo install trunk --locked     # once; the UI is built with Trunk
cargo tauri dev                  # from this folder
```

Set your Gemini API key in the app (Settings). It is stored in the Windows credential store.
`GEMINI_API_KEY` in the environment also works as a fallback.

## Look at it without Tauri or a key

`trunk serve` (from this folder), then open http://localhost:1420 in a browser. The UI runs against a
scripted mock backend that replays the protocol fixtures: messages containing "missing" show a tool
failure, "create"/"write" shows the approval dialog, "retry" shows a rate-limit retry. Add `?nokey`
to the URL to see the missing-key state.

## End-to-end self-test (live Gemini)

`LIGHTNING_GUI_SELFTEST=<plan.json>` makes the real app drive itself through its own UI callbacks and
print `SELFTEST {...}` lines, then exit. `LIGHTNING_GUI_DATA_DIR=<dir>` keeps its settings away from your
real ones. A plan is `{ "workspace": "...", "steps": [{ "message": "...", "approval": true|false,
"new_session_first": false, "stop_after_ms": 1500 }] }`.

## Tests

`cargo test --workspace` covers the shell logic, the UI reducer/formatters and everything below them.
