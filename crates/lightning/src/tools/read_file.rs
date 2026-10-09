use std::fs;
use std::path::Path;

use schemars::JsonSchema;
use serde::Deserialize;

use crate::tools::ToolOutput;
use crate::tools::error::ToolError;
use crate::tools::workspace;

/// Returns the text contents of a file inside the workspace. Paths must be relative to the workspace root; anything outside the workspace is forbidden. Use offset and limit to read part of a large file.
#[derive(Deserialize, Debug, JsonSchema)]
pub struct ReadFileArgs {
    /// Path of the file to read, relative to the workspace root.
    pub file_path: String,
    /// First line to return, counting from 0. Defaults to 0 (start of the file).
    #[serde(default)]
    pub offset: u32,
    /// Maximum number of lines to return. 0 (the default) means all remaining lines.
    #[serde(default)]
    pub limit: u32,
}

pub fn read_file(work_space: &workspace::Workspace, args: ReadFileArgs) -> Result<ToolOutput, ToolError> {
    let target: &Path = args.file_path.as_ref();
    let canonical_target = work_space.resolve(target)?;

    let bytes = fs::read(&canonical_target).map_err(|err| ToolError::Io {
        message: format!("Failed reading {}: {err}", target.display()),
    })?;
    let text = String::from_utf8(bytes).map_err(|_| ToolError::Io {
        message: format!("{} is not valid UTF-8 text (binary file?)", target.display()),
    })?;

    let total_lines = text.lines().count();
    let offset = args.offset as usize;
    let content = if offset == 0 && args.limit == 0 {
        text
    } else {
        let take = if args.limit == 0 { usize::MAX } else { args.limit as usize };
        text.lines().skip(offset).take(take).collect::<Vec<_>>().join("\n")
    };
    let returned_lines = content.lines().count();

    Ok(ToolOutput {
        content,
        metadata: serde_json::json!({
            "line_count": total_lines,
            "offset": offset,
            "returned_lines": returned_lines,
        }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::test_support::temp_workspace;

    fn args(path: &str, offset: u32, limit: u32) -> ReadFileArgs {
        ReadFileArgs { file_path: path.into(), offset, limit }
    }

    #[test]
    fn reads_whole_file() {
        let (ws, dir) = temp_workspace();
        fs::write(dir.join("a.txt"), "one\ntwo\nthree\n").unwrap();
        let out = read_file(&ws, args("a.txt", 0, 0)).unwrap();
        assert_eq!(out.content, "one\ntwo\nthree\n");
        assert_eq!(out.metadata["line_count"], 3);
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn reads_a_line_window() {
        let (ws, dir) = temp_workspace();
        fs::write(dir.join("a.txt"), "one\ntwo\nthree\nfour\n").unwrap();
        let out = read_file(&ws, args("a.txt", 1, 2)).unwrap();
        assert_eq!(out.content, "two\nthree");
        assert_eq!(out.metadata["returned_lines"], 2);
        assert_eq!(out.metadata["line_count"], 4);
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn missing_file_is_not_found_and_binary_is_rejected() {
        let (ws, dir) = temp_workspace();
        assert!(matches!(read_file(&ws, args("nope", 0, 0)), Err(ToolError::NotFound { .. })));
        fs::write(dir.join("bin"), [0xff, 0xfe, 0x00]).unwrap();
        let err = read_file(&ws, args("bin", 0, 0)).unwrap_err();
        assert!(err.to_string().contains("not valid UTF-8"));
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn directory_is_an_io_error() {
        let (ws, dir) = temp_workspace();
        fs::create_dir(dir.join("d")).unwrap();
        assert!(matches!(read_file(&ws, args("d", 0, 0)), Err(ToolError::Io { .. })));
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn args_default_when_omitted() {
        let a: ReadFileArgs = serde_json::from_str(r#"{"file_path":"x"}"#).unwrap();
        assert_eq!((a.offset, a.limit), (0, 0));
    }
}
