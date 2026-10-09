use std::fmt;
use std::io;

use crate::turn::ErrorClass;

#[derive(Debug)]
pub enum ToolError {
    NotFound{path: String},
    PathOutsideProject{path: String},
    InvalidArguments{message: String},
    //ExecutionFailed{command: String, exit_code: i32, stderr: String},
    //Timeout{command: String, duration: i32},
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
            ToolError::InvalidArguments { .. } => ErrorClass::InvalidArguments,
            ToolError::Io { .. } => ErrorClass::Io,
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
                write!(f, "The file path: {path} is not found")
            }
            ToolError::PathOutsideProject { path } => {
                write!(f, "The file path: {path} is outside the working directory. You do not have access to this")
            }
            ToolError::Io { message } => {
                write!(f, "I/O error occurred: {message}")
            }
            ToolError::InvalidArguments { message } => {
                write!(f, "Invalid tool arguments or tool name used: {message}")
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
