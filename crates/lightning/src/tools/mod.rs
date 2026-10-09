use serde::Serialize;

pub mod dispatch;
pub mod error;
pub mod gate;
pub mod list_dir;
pub mod read_file;
pub mod registry;
pub mod workspace;
pub mod write_file;

#[derive(Serialize, Debug, Clone)]
pub struct ToolOutput {
    pub content: String,
    pub metadata: serde_json::Value,
}

pub use error::ToolError;
pub use list_dir::list_dir;
pub use read_file::read_file;
pub use write_file::write_file;

#[cfg(test)]
pub(crate) mod test_support {
    use std::fs;
    use std::path::PathBuf;

    use super::workspace::Workspace;

    /// A fresh workspace in a unique temp directory. The caller removes `dir`.
    pub fn temp_workspace() -> (Workspace, PathBuf) {
        let dir = std::env::temp_dir().join(format!("lightning-ws-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let ws = Workspace::new(&dir).unwrap();
        (ws, dir)
    }
}
