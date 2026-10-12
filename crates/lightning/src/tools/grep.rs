use std::fs;

use ignore::WalkBuilder;
use ignore::overrides::OverrideBuilder;
use regex::RegexBuilder;
use schemars::JsonSchema;
use serde::Deserialize;

use crate::tools::ToolOutput;
use crate::tools::error::ToolError;
use crate::tools::workspace::{self, Workspace};

const HARD_MAX_RESULTS: u32 = 500;
const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_LINE_CHARS: usize = 200;

fn dot() -> String {
    ".".to_string()
}

fn default_max() -> u32 {
    100
}

/// Searches the text of files in the workspace for a regular expression and returns matching lines as `path:line: text`. Skips files ignored by .gitignore, hidden files, binary files and very large files. Use it to find where something is defined or used before reading files.
#[derive(Deserialize, Debug, JsonSchema)]
pub struct GrepArgs {
    /// Regular expression to look for, in Rust regex syntax (for example `fn \w+` or `TODO`).
    pub pattern: String,
    /// Folder or file to search, relative to the workspace root. Defaults to the whole workspace (".").
    #[serde(default = "dot")]
    pub path: String,
    /// Only search files whose name matches this glob, for example `*.rs`. Empty (the default) searches every file.
    #[serde(default)]
    pub include: String,
    /// Match upper and lower case alike. Defaults to false.
    #[serde(default)]
    pub ignore_case: bool,
    /// Maximum number of matching lines to return. Defaults to 100; at most 500.
    #[serde(default = "default_max")]
    pub max_results: u32,
}

pub fn grep(ws: &Workspace, args: GrepArgs) -> Result<ToolOutput, ToolError> {
    let start = ws.resolve(&args.path)?;
    let limit = args.max_results.clamp(1, HARD_MAX_RESULTS) as usize;

    let re = RegexBuilder::new(&args.pattern)
        .case_insensitive(args.ignore_case)
        .size_limit(1 << 20)
        .build()
        .map_err(|e| ToolError::InvalidArguments { message: format!("bad regular expression: {e}") })?;

    let mut walk = WalkBuilder::new(&start);
    walk.require_git(false)
        .sort_by_file_name(|a, b| a.cmp(b))
        .filter_entry(|entry| !workspace::is_protected_name(&entry.file_name().to_string_lossy()));
    if !args.include.trim().is_empty() {
        let mut overrides = OverrideBuilder::new(&start);
        overrides
            .add(args.include.trim())
            .map_err(|e| ToolError::InvalidArguments { message: format!("bad glob: {e}") })?;
        walk.overrides(overrides.build().map_err(|e| ToolError::InvalidArguments { message: format!("bad glob: {e}") })?);
    }

    let mut lines = Vec::new();
    let mut files_searched = 0usize;
    let mut truncated = false;

    'files: for entry in walk.build().flatten() {
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let path = entry.path();
        if fs::metadata(path).map(|m| m.len()).unwrap_or(u64::MAX) > MAX_FILE_BYTES {
            continue;
        }
        let Ok(bytes) = fs::read(path) else { continue };
        if bytes.iter().take(8192).any(|b| *b == 0) {
            continue;
        }
        let Ok(text) = String::from_utf8(bytes) else { continue };
        files_searched += 1;

        let shown = path.strip_prefix(ws.root()).unwrap_or(path).display().to_string().replace('\\', "/");
        for (index, line) in text.lines().enumerate() {
            if re.is_match(line) {
                if lines.len() == limit {
                    truncated = true;
                    break 'files;
                }
                let trimmed: String = line.trim().chars().take(MAX_LINE_CHARS).collect();
                lines.push(format!("{shown}:{}: {trimmed}", index + 1));
            }
        }
    }

    let matches = lines.len();
    let mut content = if lines.is_empty() { "No matches.".to_string() } else { lines.join("\n") };
    if truncated {
        content.push_str(&format!("\n[stopped after {limit} matches; narrow the pattern, path or include to see the rest]"));
    }
    Ok(ToolOutput {
        content,
        metadata: serde_json::json!({ "matches": matches, "files_searched": files_searched, "truncated": truncated }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::test_support::temp_workspace;

    fn args(pattern: &str) -> GrepArgs {
        GrepArgs { pattern: pattern.into(), path: ".".into(), include: String::new(), ignore_case: false, max_results: 100 }
    }

    fn setup() -> (Workspace, std::path::PathBuf) {
        let (ws, dir) = temp_workspace();
        fs::create_dir_all(dir.join("src/inner")).unwrap();
        fs::write(dir.join("src/main.rs"), "fn main() {\n    helper();\n}\n// TODO: tidy\n").unwrap();
        fs::write(dir.join("src/inner/lib.rs"), "pub fn helper() {}\n").unwrap();
        fs::write(dir.join("notes.txt"), "todo: buy milk\nhelper notes\n").unwrap();
        (ws, dir)
    }

    #[test]
    fn finds_matching_lines_with_paths_and_numbers() {
        let (ws, dir) = setup();
        let out = grep(&ws, args("helper")).unwrap();
        let lines: Vec<&str> = out.content.lines().collect();
        assert_eq!(lines, vec!["notes.txt:2: helper notes", "src/inner/lib.rs:1: pub fn helper() {}", "src/main.rs:2: helper();"]);
        assert_eq!(out.metadata["matches"], 3);
        assert_eq!(out.metadata["truncated"], false);
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn case_flag_include_glob_and_path_scope() {
        let (ws, dir) = setup();
        assert_eq!(grep(&ws, args("TODO")).unwrap().metadata["matches"], 1);
        let insensitive = grep(&ws, GrepArgs { ignore_case: true, ..args("todo") }).unwrap();
        assert_eq!(insensitive.metadata["matches"], 2);

        let only_rs = grep(&ws, GrepArgs { include: "*.rs".into(), ..args("helper") }).unwrap();
        assert!(only_rs.content.lines().all(|l| l.contains(".rs:")));
        assert_eq!(only_rs.metadata["matches"], 2);

        let scoped = grep(&ws, GrepArgs { path: "src/inner".into(), ..args("helper") }).unwrap();
        assert_eq!(scoped.content, "src/inner/lib.rs:1: pub fn helper() {}");
        let single = grep(&ws, GrepArgs { path: "notes.txt".into(), ..args("milk") }).unwrap();
        assert_eq!(single.content, "notes.txt:1: todo: buy milk");
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn no_matches_and_result_limit() {
        let (ws, dir) = setup();
        assert_eq!(grep(&ws, args("zzz_nothing")).unwrap().content, "No matches.");
        fs::write(dir.join("many.txt"), "x\n".repeat(50)).unwrap();
        let out = grep(&ws, GrepArgs { max_results: 5, path: "many.txt".into(), ..args("x") }).unwrap();
        assert_eq!(out.metadata["matches"], 5);
        assert_eq!(out.metadata["truncated"], true);
        assert!(out.content.contains("stopped after 5 matches"));
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn skips_ignored_hidden_protected_binary_and_huge_files() {
        let (ws, dir) = setup();
        fs::write(dir.join(".gitignore"), "ignored.txt\n").unwrap();
        fs::write(dir.join("ignored.txt"), "helper in ignored\n").unwrap();
        fs::write(dir.join(".env"), "helper=secret\n").unwrap();
        fs::create_dir_all(dir.join(".lightning/sessions")).unwrap();
        fs::write(dir.join(".lightning/sessions/events.jsonl"), "helper in logs\n").unwrap();
        fs::write(dir.join("blob.bin"), [b'h', b'e', b'l', b'p', b'e', b'r', 0, 1, 2]).unwrap();
        fs::write(dir.join("big.txt"), format!("helper {}", "a".repeat(3 * 1024 * 1024))).unwrap();

        let out = grep(&ws, args("helper")).unwrap();
        for hidden in ["ignored.txt", ".env", ".lightning", "blob.bin", "big.txt"] {
            assert!(!out.content.contains(hidden), "{hidden} leaked into:\n{}", out.content);
        }
        assert_eq!(out.metadata["matches"], 3);
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn explicit_protected_or_outside_paths_are_refused() {
        let (ws, dir) = setup();
        fs::write(dir.join(".env"), "x\n").unwrap();
        assert!(matches!(grep(&ws, GrepArgs { path: ".env".into(), ..args("x") }), Err(ToolError::Blocked { .. })));
        assert!(matches!(grep(&ws, GrepArgs { path: "..".into(), ..args("x") }), Err(ToolError::PathOutsideProject { .. })));
        assert!(matches!(grep(&ws, GrepArgs { path: "nope".into(), ..args("x") }), Err(ToolError::NotFound { .. })));
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn bad_regex_and_bad_glob_are_invalid_arguments() {
        let (ws, dir) = setup();
        assert!(matches!(grep(&ws, args("(unclosed")), Err(ToolError::InvalidArguments { .. })));
        assert!(matches!(grep(&ws, GrepArgs { include: "[".into(), ..args("x") }), Err(ToolError::InvalidArguments { .. })));
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn long_lines_are_trimmed_and_defaults_apply() {
        let (ws, dir) = setup();
        fs::write(dir.join("long.txt"), format!("needle {}\n", "z".repeat(1000))).unwrap();
        let out = grep(&ws, GrepArgs { path: "long.txt".into(), ..args("needle") }).unwrap();
        assert!(out.content.len() < 260);
        let a: GrepArgs = serde_json::from_str(r#"{"pattern":"x"}"#).unwrap();
        assert_eq!((a.path.as_str(), a.max_results, a.ignore_case, a.include.as_str()), (".", 100, false, ""));
        fs::remove_dir_all(dir).ok();
    }
}
