use std::fs;

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use crate::tools::ToolOutput;
use crate::tools::error::ToolError;
use crate::tools::workspace;

fn workspace_root() -> String {
    ".".to_string()
}

/// Lists the entries of a folder inside the workspace, one per line; directories end with a slash. Defaults to the workspace root. Paths outside the workspace are forbidden.
#[derive(Deserialize, Debug, JsonSchema)]
pub struct ListDirArgs {
    /// Folder to list, relative to the workspace root. Defaults to the root (".").
    #[serde(default = "workspace_root")]
    pub dir_path: String,
}

pub fn list_dir(work_space: &workspace::Workspace, args: ListDirArgs) -> Result<ToolOutput, ToolError> {
    let canonical_target = work_space.resolve(&args.dir_path)?;

    let mut entries = Vec::new();
    for entry in fs::read_dir(&canonical_target)? {
        let entry = entry?;
        let mut name = entry.file_name().to_string_lossy().into_owned();
        if entry.file_type()?.is_dir() {
            name.push('/');
        }
        entries.push(name);
    }
    entries.sort();

    let mut folder_contents = String::new();
    for entry in &entries {
        folder_contents += entry;
        folder_contents += "\n";
    }

    Ok(ToolOutput { content: folder_contents, metadata: json!({ "item_count": entries.len() }) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::test_support::temp_workspace;

    #[test]
    fn lists_sorted_with_dir_markers() {
        let (ws, dir) = temp_workspace();
        fs::write(dir.join("b.txt"), "").unwrap();
        fs::write(dir.join("a.txt"), "").unwrap();
        fs::create_dir(dir.join("sub")).unwrap();
        let out = list_dir(&ws, ListDirArgs { dir_path: ".".into() }).unwrap();
        assert_eq!(out.content, "a.txt\nb.txt\nsub/\n");
        assert_eq!(out.metadata["item_count"], 3);
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn default_is_workspace_root_and_errors_are_typed() {
        let (ws, dir) = temp_workspace();
        let a: ListDirArgs = serde_json::from_str("{}").unwrap();
        assert_eq!(a.dir_path, ".");
        assert!(list_dir(&ws, a).is_ok());
        assert!(matches!(
            list_dir(&ws, ListDirArgs { dir_path: "missing".into() }),
            Err(ToolError::NotFound { .. })
        ));
        assert!(matches!(
            list_dir(&ws, ListDirArgs { dir_path: "..".into() }),
            Err(ToolError::PathOutsideProject { .. })
        ));
        fs::remove_dir_all(dir).ok();
    }
}
