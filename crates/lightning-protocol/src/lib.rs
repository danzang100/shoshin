//! Wire types shared by the lightning core, the Tauri shell and the wasm UI.
//! No tokio, reqwest or other native-only dependencies, so it builds for wasm.
pub mod dto;
pub mod event;
pub mod fixtures;
pub mod summary;
pub mod usage;
pub mod view;

pub use event::{Event, EventKind, Outcome, SCHEMA_VERSION, SessionStatus, ToolUseRef, preview};
pub use summary::Summary;
pub use usage::{FinishReason, Usage};
pub use view::TimelineModel;
