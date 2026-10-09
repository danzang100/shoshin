use std::fs;

use schemars::JsonSchema;
use serde::Deserialize;

use crate::tools::ToolOutput;
use crate::tools::error::ToolError;
use crate::tools::workspace;

/// Largest file this tool will write.
const MAX_WRITE_BYTES: usize = 1024 * 1024;

/// Creates a text file inside the workspace, or overwrites it completely if it exists. Missing parent folders are created. Paths must be relative to the workspace root; anything outside the workspace is forbidden. This changes files on disk and may require the user's approval.
#[derive(Deserialize, Debug, JsonSchema)]
pub struct WriteFileArgs {
    /// Path of the file to write, relative to the workspace root.
    pub file_path: String,
    /// The complete new contents of the file.
    pub content: String,
}

pub fn write_file(work_space: &workspace::Workspace, args: WriteFileArgs) -> Result<ToolOutput, ToolError> {
    if args.content.len() > MAX_WRITE_BYTES {
        return Err(ToolError::InvalidArguments {
            message: format!("content is {} bytes; the limit is {MAX_WRITE_BYTES}", args.content.len()),
        });
    }
    let target = work_space.resolve_for_write(&args.file_path)?;
    let existed = target.exists();

    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).map_err(|err| ToolError::Io {
            message: format!("Failed creating folders for {}: {err}", args.file_path),
        })?;
    }
    fs::write(&target, args.content.as_bytes()).map_err(|err| ToolError::Io {
        message: format!("Failed writing {}: {err}", args.file_path),
    })?;

    Ok(ToolOutput {
        content: format!(
            "{} {} ({} bytes)",
            if existed { "Overwrote" } else { "Created" },
            args.file_path,
            args.content.len()
        ),
        metadata: serde_json::json!({ "bytes": args.content.len(), "created": !existed }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::test_support::temp_workspace;

    fn args(path: &str, content: &str) -> WriteFileArgs {
        WriteFileArgs { file_path: path.into(), content: content.into() }
    }

    #[test]
    fn creates_then_overwrites() {
        let (ws, dir) = temp_workspace();
        let out = write_file(&ws, args("deep/notes.txt", "hello")).unwrap();
        assert_eq!(out.metadata["created"], true);
        assert_eq!(fs::read_to_string(dir.join("deep/notes.txt")).unwrap(), "hello");

        let out = write_file(&ws, args("deep/notes.txt", "bye")).unwrap();
        assert_eq!(out.metadata["created"], false);
        assert!(out.content.starts_with("Overwrote"));
        assert_eq!(fs::read_to_string(dir.join("deep/notes.txt")).unwrap(), "bye");
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn refuses_outside_workspace_and_writes_nothing() {
        let (ws, dir) = temp_workspace();
        let escape = format!("../{}-escape.txt", uuid::Uuid::new_v4());
        let err = write_file(&ws, args(&escape, "x")).unwrap_err();
        assert!(matches!(err, ToolError::PathOutsideProject { .. }));
        assert!(!dir.parent().unwrap().join(&escape[3..]).exists());
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn refuses_oversized_content_and_directories() {
        let (ws, dir) = temp_workspace();
        let big = "x".repeat(MAX_WRITE_BYTES + 1);
        assert!(matches!(write_file(&ws, args("big.txt", &big)), Err(ToolError::InvalidArguments { .. })));
        fs::create_dir(dir.join("d")).unwrap();
        assert!(matches!(write_file(&ws, args("d", "x")), Err(ToolError::InvalidArguments { .. })));
        fs::remove_dir_all(dir).ok();
    }
}
