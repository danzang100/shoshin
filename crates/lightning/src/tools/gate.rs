//! Minimal permission gate (spec 001, section 7.3). Called only from dispatch.
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::obs::preview;
use crate::tools::registry::ToolKind;
use crate::turn::ToolUse;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionMode {
    /// Ask the user for each state-changing call.
    #[default]
    Ask,
    Allow,
    Deny,
}

/// What the user is asked to approve.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalRequest {
    pub tool: String,
    pub summary: String,
}

/// Boxed future so `Prompter` stays object-safe.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Source of yes/no answers. The CLI implements this with a terminal prompt, the
/// GUI with a dialog. The answer may take minutes; dispatch races this future against
/// cancellation and drops it when cancelled, so implementations must clean up in `Drop`
/// (for example close the dialog).
pub trait Prompter: Send + Sync {
    fn confirm<'a>(&'a self, request: &'a ApprovalRequest) -> BoxFuture<'a, bool>;
}

/// Answers every request the same way. `FixedPrompter(false)` is the
/// non-interactive prompter: `ask` mode resolves to deny.
pub struct FixedPrompter(pub bool);

impl Prompter for FixedPrompter {
    fn confirm<'a>(&'a self, _request: &'a ApprovalRequest) -> BoxFuture<'a, bool> {
        Box::pin(std::future::ready(self.0))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    pub allowed: bool,
    pub reason: String,
}

pub struct Gate {
    mode: PermissionMode,
    prompter: Arc<dyn Prompter>,
}

impl Gate {
    pub fn new(mode: PermissionMode, prompter: Arc<dyn Prompter>) -> Self {
        Gate { mode, prompter }
    }

    pub async fn check(&self, kind: ToolKind, tool_use: &ToolUse) -> Decision {
        if !kind.needs_approval() {
            return Decision { allowed: true, reason: "read-only tool".into() };
        }
        match self.mode {
            PermissionMode::Allow => Decision { allowed: true, reason: "permission mode: allow".into() },
            PermissionMode::Deny => Decision { allowed: false, reason: "permission mode: deny".into() },
            PermissionMode::Ask => {
                let request = ApprovalRequest { tool: tool_use.name.clone(), summary: describe(&tool_use.name, &tool_use.arguments) };
                if self.prompter.confirm(&request).await {
                    Decision { allowed: true, reason: "approved by user".into() }
                } else {
                    Decision { allowed: false, reason: "declined by user".into() }
                }
            }
        }
    }
}

fn describe(name: &str, args: &Value) -> String {
    match name {
        "write_file" => {
            let path = args["file_path"].as_str().unwrap_or("?");
            let bytes = args["content"].as_str().map(str::len).unwrap_or(0);
            format!("write {path} ({bytes} bytes)")
        }
        _ => preview(&args.to_string(), 200),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Mutex;

    fn write_use() -> ToolUse {
        ToolUse { id: "c".into(), name: "write_file".into(), arguments: json!({ "file_path": "a.txt", "content": "hey" }) }
    }

    struct Recording(Mutex<Vec<ApprovalRequest>>);

    impl Prompter for Recording {
        fn confirm<'a>(&'a self, request: &'a ApprovalRequest) -> BoxFuture<'a, bool> {
            self.0.lock().unwrap().push(request.clone());
            Box::pin(std::future::ready(true))
        }
    }

    #[tokio::test]
    async fn read_only_tools_never_prompt() {
        let gate = Gate::new(PermissionMode::Deny, Arc::new(FixedPrompter(false)));
        assert!(gate.check(ToolKind::ReadFile, &write_use()).await.allowed);
    }

    #[tokio::test]
    async fn modes_decide_for_state_changing_tools() {
        let tu = write_use();
        let allow = Gate::new(PermissionMode::Allow, Arc::new(FixedPrompter(false)));
        let deny = Gate::new(PermissionMode::Deny, Arc::new(FixedPrompter(true)));
        assert!(allow.check(ToolKind::WriteFile, &tu).await.allowed);
        assert!(!deny.check(ToolKind::WriteFile, &tu).await.allowed);
    }

    #[tokio::test]
    async fn ask_defers_to_the_prompter_with_a_readable_summary() {
        let rec = Arc::new(Recording(Mutex::new(Vec::new())));
        let gate = Gate::new(PermissionMode::Ask, rec.clone());
        assert!(gate.check(ToolKind::WriteFile, &write_use()).await.allowed);
        assert_eq!(rec.0.lock().unwrap()[0].summary, "write a.txt (3 bytes)");

        let no = Gate::new(PermissionMode::Ask, Arc::new(FixedPrompter(false)));
        let d = no.check(ToolKind::WriteFile, &write_use()).await;
        assert!(!d.allowed);
        assert_eq!(d.reason, "declined by user");
    }
}
