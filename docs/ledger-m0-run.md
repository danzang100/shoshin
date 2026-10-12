# Model Ledger M0: running on your real logs (L8, not yet done)

Status: not run. Everything so far was tested on synthetic fixtures only.

`ledger` only ever opens transcripts read-only and refuses a database path inside the transcript root. To be extra safe, work on a copy:

```powershell
robocopy "$env:USERPROFILE\.claude\projects" "$env:USERPROFILE\ledger-snapshot" /E /COPY:D /R:1 /W:1
attrib +R "$env:USERPROFILE\ledger-snapshot\*" /S
cargo run -p ledger --release -- ingest --root "$env:USERPROFILE\ledger-snapshot"
cargo run -p ledger --release -- doctor
cargo run -p ledger --release -- report --by model
```

Before the report is useful, copy `crates/ledger/pricing/default.toml` to `%LOCALAPPDATA%\ledger\pricing.toml` and fill the prices from the provider's pricing page. Unpriced models show `unpriced`.

After the run, record counts, timing and any parse problems here (no private content).
