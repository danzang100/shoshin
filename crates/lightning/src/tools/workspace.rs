use std::fs;
use std::io::ErrorKind;
use std::path::{Component, Path, PathBuf};

use crate::tools::ToolError;

/// The directory tools are allowed to touch. Every path a tool receives goes
/// through [`Workspace::resolve`] or [`Workspace::resolve_for_write`].
#[derive(Debug, Clone)]
pub struct Workspace {
    root: PathBuf,
}

impl Workspace {
    pub fn new(root: impl AsRef<Path>) -> Result<Self, ToolError> {
        let canonical_root = fs::canonicalize(root.as_ref())?;
        Ok(Workspace { root: canonical_root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Root for showing to people and the model: without the Windows verbatim path prefix.
    pub fn display_root(&self) -> String {
        let text = self.root.display().to_string();
        text.strip_prefix(r"\\?\").map(str::to_string).unwrap_or(text)
    }

    /// Resolves an existing path. Fails if it does not exist or escapes the workspace.
    pub fn resolve(&self, target_path: impl AsRef<Path>) -> Result<PathBuf, ToolError> {
        let target = target_path.as_ref();

        // Reject lexical escapes first, so `../missing` reports "outside" and not "not found".
        let normalised = normalise(&self.root.join(target));
        self.ensure_inside(&normalised, target)?;
        self.ensure_not_protected(&normalised, target)?;

        let canonical_target = fs::canonicalize(&normalised).map_err(|err| match err.kind() {
            ErrorKind::PermissionDenied => ToolError::Io {
                message: format!("Permission denied accessing: {}", target.display()),
            },
            ErrorKind::NotFound => ToolError::NotFound { path: target.display().to_string() },
            _ => ToolError::Other(err),
        })?;

        self.ensure_inside(&canonical_target, target)?;
        Ok(canonical_target)
    }

    /// Resolves a path that may not exist yet (for writing). `..` is folded lexically,
    /// then the nearest existing ancestor is canonicalized so symlinks and junctions
    /// cannot lead outside the workspace.
    pub fn resolve_for_write(&self, target_path: impl AsRef<Path>) -> Result<PathBuf, ToolError> {
        let target = target_path.as_ref();
        let normalised = normalise(&self.root.join(target));
        self.ensure_inside(&normalised, target)?;
        self.ensure_not_protected(&normalised, target)?;

        // Walk up to the nearest ancestor that exists, remembering what was cut off.
        let mut existing = normalised.as_path();
        let mut missing: Vec<&std::ffi::OsStr> = Vec::new();
        while !existing.exists() {
            match (existing.file_name(), existing.parent()) {
                (Some(name), Some(parent)) => {
                    missing.push(name);
                    existing = parent;
                }
                _ => break,
            }
        }

        let mut resolved = fs::canonicalize(existing).map_err(|err| match err.kind() {
            ErrorKind::PermissionDenied => ToolError::Io {
                message: format!("Permission denied accessing: {}", target.display()),
            },
            _ => ToolError::Other(err),
        })?;
        resolved.extend(missing.iter().rev());

        self.ensure_inside(&resolved, target)?;
        if resolved.is_dir() {
            return Err(ToolError::InvalidArguments {
                message: format!("{} is a directory, not a file", target.display()),
            });
        }
        Ok(resolved)
    }

    /// Secrets (`.env`, `.env.*` except examples) and the harness's own `.lightning`
    /// folder must never reach the model, even though they live in the workspace.
    fn ensure_not_protected(&self, normalised: &Path, original: &Path) -> Result<(), ToolError> {
        let relative = normalised.strip_prefix(&self.root).unwrap_or(normalised);
        let blocked = relative.components().any(|c| match c {
            Component::Normal(name) => is_protected_name(&name.to_string_lossy()),
            _ => false,
        });
        if blocked {
            Err(ToolError::Blocked { path: original.display().to_string() })
        } else {
            Ok(())
        }
    }

    fn ensure_inside(&self, canonical: &Path, original: &Path) -> Result<(), ToolError> {
        if canonical.starts_with(&self.root) {
            Ok(())
        } else {
            Err(ToolError::PathOutsideProject { path: original.display().to_string() })
        }
    }
}

fn is_protected_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    if lower == ".lightning" || lower == ".env" {
        return true;
    }
    lower.starts_with(".env.") && !(lower.ends_with(".example") || lower.ends_with(".sample") || lower.ends_with(".template"))
}

/// Folds `.` and `..` without touching the filesystem.
fn normalise(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                // Popping past the root is a no-op, like on a real filesystem.
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::test_support::temp_workspace;

    #[test]
    fn resolve_finds_existing_and_rejects_missing() {
        let (ws, dir) = temp_workspace();
        fs::write(dir.join("a.txt"), "x").unwrap();
        assert!(ws.resolve("a.txt").is_ok());
        assert!(matches!(ws.resolve("nope.txt"), Err(ToolError::NotFound { .. })));
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn resolve_rejects_parent_traversal() {
        let (ws, dir) = temp_workspace();
        let outside = dir.parent().unwrap().to_path_buf();
        assert!(outside.exists());
        let name = dir.file_name().unwrap().to_string_lossy().into_owned();
        // `../<this dir>` exists but is the workspace itself; `..` alone is outside.
        assert!(ws.resolve(format!("../{name}")).is_ok());
        assert!(matches!(ws.resolve(".."), Err(ToolError::PathOutsideProject { .. })));
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn resolve_rejects_absolute_path_outside() {
        let (ws, dir) = temp_workspace();
        let outside = dir.parent().unwrap();
        assert!(matches!(ws.resolve(outside), Err(ToolError::PathOutsideProject { .. })));
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn resolve_for_write_allows_new_nested_paths() {
        let (ws, dir) = temp_workspace();
        let p = ws.resolve_for_write("new/dir/file.txt").unwrap();
        assert!(p.starts_with(ws.root()));
        assert!(p.ends_with("file.txt"));
        assert!(!p.exists());
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn resolve_for_write_rejects_escapes() {
        let (ws, dir) = temp_workspace();
        assert!(matches!(ws.resolve_for_write("../x.txt"), Err(ToolError::PathOutsideProject { .. })));
        assert!(matches!(ws.resolve_for_write("a/../../x.txt"), Err(ToolError::PathOutsideProject { .. })));
        let abs = dir.parent().unwrap().join("elsewhere.txt");
        assert!(matches!(ws.resolve_for_write(&abs), Err(ToolError::PathOutsideProject { .. })));
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn resolve_for_write_allows_inner_dotdot_and_rejects_directories() {
        let (ws, dir) = temp_workspace();
        fs::create_dir(dir.join("sub")).unwrap();
        assert!(ws.resolve_for_write("sub/../ok.txt").is_ok());
        assert!(matches!(ws.resolve_for_write("sub"), Err(ToolError::InvalidArguments { .. })));
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn secrets_and_harness_logs_are_blocked_for_read_and_write() {
        let (ws, dir) = temp_workspace();
        fs::write(dir.join(".env"), "GEMINI_API_KEY=x").unwrap();
        fs::create_dir(dir.join(".lightning")).unwrap();
        fs::write(dir.join(".env.example"), "KEY=").unwrap();
        for path in [".env", ".ENV", ".env.local", ".lightning", ".lightning/sessions", "sub/../.env"] {
            assert!(matches!(ws.resolve(path), Err(ToolError::Blocked { .. })), "read {path}");
        }
        assert!(matches!(ws.resolve_for_write(".env"), Err(ToolError::Blocked { .. })));
        assert!(matches!(ws.resolve_for_write(".lightning/config.toml"), Err(ToolError::Blocked { .. })));
        assert!(matches!(ws.resolve_for_write("a/.env.production"), Err(ToolError::Blocked { .. })));
        assert!(ws.resolve(".env.example").is_ok());
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn display_root_has_no_verbatim_prefix() {
        let (ws, dir) = temp_workspace();
        assert!(!ws.display_root().starts_with(r"\\?\"));
        assert!(ws.display_root().ends_with(dir.file_name().unwrap().to_str().unwrap()));
        fs::remove_dir_all(dir).ok();
    }
}
