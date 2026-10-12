use std::fmt;
use std::io;

use crate::turn::ErrorClass;

#[derive(Debug)]
pub enum ToolError {
    NotFound{path: String},
    PathOutsideProject{path: String},
    /// Inside the workspace but off limits (secrets, harness logs).
    Blocked{path: String},
    InvalidArguments{message: String},
    /// A shell command exited with a nonzero code; `output` is what it printed.
    ExecutionFailed{exit_code: i32, output: String},
    /// A shell command was killed after `seconds`; `output` is what it printed so far.
    Timeout{seconds: u64, output: String},
    Cancelled,
    Io{message: String},
    Other(io::Error),
}

impl std::error::Error for ToolError {}

impl ToolError {
    /// Category shown to the model and recorded in logs.
    pub fn class(&self) -> ErrorClass {
        match self {
            ToolError::NotFound { .. } => ErrorClass::NotFound,
            ToolError::PathOutsideProject { .. } => ErrorClass::OutsideWorkspace,
            ToolError::Blocked { .. } => ErrorClass::PermissionDenied,
            ToolError::InvalidArguments { .. } => ErrorClass::InvalidArguments,
            ToolError::Io { .. } => ErrorClass::Io,
            ToolError::ExecutionFailed { .. } => ErrorClass::ExecutionFailed,
            ToolError::Timeout { .. } => ErrorClass::Timeout,
            ToolError::Cancelled => ErrorClass::Internal,
            ToolError::Other(err) => match err.kind() {
                io::ErrorKind::NotFound => ErrorClass::NotFound,
                io::ErrorKind::PermissionDenied => ErrorClass::PermissionDenied,
                _ => ErrorClass::Io,
            },
        }
    }
}

impl From<io::Error> for ToolError {
    fn from(err: io::Error) -> Self {
        ToolError::Other(err)
    }
}

impl From<serde_json::Error> for ToolError {
    fn from(err: serde_json::Error) -> Self {
        ToolError::InvalidArguments{
            message: err.to_string()
        }
    }
}

impl fmt::Display for ToolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ToolError::NotFound {path} => {
                write!(f, "Not found: {path}. Use list_dir to check the path.")
            }
            ToolError::PathOutsideProject { path } => {
                write!(f, "Path {path} is outside the workspace. Only paths inside the workspace are allowed.")
            }
            ToolError::Blocked { path } => {
                write!(f, "Access to {path} is blocked because it may hold secrets or harness logs. Do not try to read or change it.")
            }
            ToolError::ExecutionFailed { exit_code, output } => {
                write!(f, "The command exited with code {exit_code}.")?;
                if !output.is_empty() {
                    write!(f, "
{output}")?;
                }
                Ok(())
            }
            ToolError::Timeout { seconds, output } => {
                write!(f, "The command did not finish within {seconds} seconds and was killed.")?;
                if !output.is_empty() {
                    write!(f, "
Output so far:
{output}")?;
                }
                Ok(())
            }
            ToolError::Cancelled => write!(f, "Cancelled by the user."),
            ToolError::Io { message } => {
                write!(f, "I/O error occurred: {message}")
            }
            ToolError::InvalidArguments { message } => {
                write!(f, "Invalid tool arguments: {message}")
            }
            ToolError::Other(err) => {
                write!(f, "Some error occurred: {}", err)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classes_map_as_specified() {
        assert_eq!(ToolError::NotFound { path: "x".into() }.class(), ErrorClass::NotFound);
        assert_eq!(
            ToolError::PathOutsideProject { path: "..".into() }.class(),
            ErrorClass::OutsideWorkspace
        );
        assert_eq!(
            ToolError::InvalidArguments { message: "m".into() }.class(),
            ErrorClass::InvalidArguments
        );
        assert_eq!(ToolError::Io { message: "m".into() }.class(), ErrorClass::Io);
        assert_eq!(ToolError::Blocked { path: ".env".into() }.class(), ErrorClass::PermissionDenied);
        assert_eq!(ToolError::ExecutionFailed { exit_code: 2, output: String::new() }.class(), ErrorClass::ExecutionFailed);
        assert_eq!(ToolError::Timeout { seconds: 5, output: String::new() }.class(), ErrorClass::Timeout);
        assert_eq!(ToolError::Cancelled.class(), ErrorClass::Internal);
        assert_eq!(
            ToolError::from(io::Error::from(io::ErrorKind::PermissionDenied)).class(),
            ErrorClass::PermissionDenied
        );
        assert_eq!(
            ToolError::from(io::Error::from(io::ErrorKind::NotFound)).class(),
            ErrorClass::NotFound
        );
        assert_eq!(
            ToolError::from(io::Error::from(io::ErrorKind::TimedOut)).class(),
            ErrorClass::Io
        );
    }
}
