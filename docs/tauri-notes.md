# Tauri 2 + Yew: confirmed facts (G0 spike)

Spiked 2026-10-10 on Windows 11 with the code in `gui/` (self-test in `gui/src/app.rs`, commands in `gui/src-tauri/src/lib.rs`). Results below were observed in a real `cargo tauri dev` window, not read from documentation.

## Versions

| Piece | Version |
|---|---|
| `cargo-tauri` CLI | 2.11.4 |
| `tauri` crate | 2.12.2 (resolved; the template lock said 2.11.5) |
| `tauri-plugin-dialog` | 2.8.1 (pulls in `tauri-plugin-fs` as a dependency; it is inert unless a capability grants fs permissions) |
| `trunk` | 0.21.14 (installed with `cargo install trunk --locked`, about 10 minutes to build) |
| `yew` | 0.21.0 (CSR) |
| `keyring` | 4.2.0 (default `v1` feature) |
| Rust | 1.98.1; target `wasm32-unknown-unknown` installed |

## Dev loop

- From `gui/`: `cargo tauri dev`. It runs `trunk serve` (port 1420), builds `lightning-gui`, and opens the window. Both processes stay alive until the window is closed.
- Rust-side `println!` appears in the `cargo tauri dev` output. The spike uses a `report` command so the webview can write results there; this is also how a headless check can read UI state.
- `cargo check -p lightning-gui-ui --target wasm32-unknown-unknown` type-checks the Yew crate without Trunk. `cargo check -p lightning-gui` checks the shell (about 2 minutes cold).

## Confirmed behaviours (all PASS in the self-test)

1. **Commands:** `invoke("ping", {name})` returns a string. Calls go through `window.__TAURI__.core.invoke` (works because `withGlobalTauri` is true); in Rust it is bound with `#[wasm_bindgen(js_namespace = ["window","__TAURI__","core"])]` and `catch` to get `Result<JsValue, JsValue>`.
2. **Managed state:** `.manage(Counter)` plus `State<'_, Counter>` in a command works; the value persists across calls.
3. **Argument names:** JS `{ sessionId, afterSeq }` arrives as Rust `session_id`, `after_seq` (camelCase to snake_case is automatic). A returned struct is serialized with its Rust field names (`session_id`, `after_seq`), **not** camelCase, unless the struct has `#[serde(rename_all = "camelCase")]`. Our DTOs should pick one convention and test it; recommendation: snake_case everywhere in DTOs and camelCase only for command argument names.
4. **Errors:** a command returning `Err(String)` rejects the JS promise with exactly that string.
5. **Events:** `app.emit("name", payload)` (needs `use tauri::Emitter`) from a background `tauri::async_runtime::spawn` task reaches `window.__TAURI__.event.listen("name", handler)`. The `listen` promise resolves to an unlisten function; calling it stops delivery (verified: no further events counted).
6. **Yew integration:** `Closure<dyn FnMut(JsValue)>` works as the listen handler. The closure must stay alive as long as the subscription (the spike keeps it until after unlisten). `use_reducer` is the right way to append log lines from async tasks (plain `use_state` would capture stale values).
7. **Core crate links:** `lightning::obs::SessionId::new()` runs inside a command. Building the shell compiles the whole core crate (reqwest, tokio, and so on), which is fine for the Rust side.
8. **Dialog plugin:** compiles and registers with `.plugin(tauri_plugin_dialog::init())`; permission `dialog:default` was added to `gui/src-tauri/capabilities/default.json`. The picker itself (`app.dialog().file().blocking_pick_folder()` inside an `async` command) needs a human click and is the one manual check: use the "Pick folder" button. Narrower permissions exist: `dialog:allow-open`, `dialog:allow-ask`, `dialog:allow-confirm`, `dialog:allow-message`, `dialog:allow-save`. Event permissions exist as `core:event:allow-listen`, `allow-unlisten`, `allow-emit`, `allow-emit-to`.
9. **Trunk config:** `[build] target = "./index.html"` in `Trunk.toml` works; `trunk serve` served the wasm bundle and Tauri loaded it from `http://localhost:1420`.
10. **keyring on Windows:** a set, get, delete round trip on the Windows Credential Manager worked (`Entry::new(service, user)`, `set_password`, `get_password`, `delete_credential`; after delete, `get_password` returns "No matching credential found"). Tested with a dummy value that was deleted immediately.

## Not verified here

- The native folder picker result (needs a click).
- Behaviour with the production CSP (the template still has `csp: null`).
- `cargo tauri build` (release bundle).
- Hot reload effects on active `listen` subscriptions (spec 002 Q7).
