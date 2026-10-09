use std::fmt;
use schemars::JsonSchema;
use schemars::schema_for;
use serde::Deserialize;
use serde::Serialize;
use strum::IntoEnumIterator;
use strum::IntoStaticStr;
use strum::{EnumIter, EnumString};

use crate::tools::list_dir::ListDirArgs;
use crate::tools::read_file;
use crate::tools::list_dir;
use crate::tools::read_file::ReadFileArgs;
use crate::tools::write_file::{self, WriteFileArgs};
use crate::tools::workspace;
use crate::tools::{ToolError, ToolOutput};


#[derive(Deserialize, Debug)]
#[serde(tag = "name", content = "arguments", rename_all = "snake_case")]
pub enum ToolCall {
    ReadFile(read_file::ReadFileArgs),
    ListDir(list_dir::ListDirArgs),
    WriteFile(write_file::WriteFileArgs),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, EnumIter, EnumString, IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum ToolKind {
    ReadFile,
    ListDir,
    WriteFile,
}

impl ToolKind {
    /// Looks a tool up by the name the model used.
    pub fn from_name(name: &str) -> Option<ToolKind> {
        name.parse().ok()
    }

    /// Tools that change state and therefore pass through the permission gate.
    pub fn needs_approval(&self) -> bool {
        match self {
            ToolKind::ReadFile | ToolKind::ListDir => false,
            ToolKind::WriteFile => true,
        }
    }

    pub fn spec(&self) -> ToolSpec {
        match self {
            ToolKind::ReadFile => { 
                spec_for::<ReadFileArgs>(self.into())
            }
            ToolKind::ListDir => {
                spec_for::<ListDirArgs>(self.into())
            }
            ToolKind::WriteFile => spec_for::<WriteFileArgs>(self.into()),
        }
    }
}

impl fmt::Display for ToolKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> fmt::Result {
        let tool_details = self.spec();
        write!(f, "{}: {}", tool_details.name, tool_details.description)
    }
}

#[derive(Serialize, Debug, Clone)]
pub struct ToolSpec {
    pub name: &'static str,
    pub description: String,
    pub parameters: serde_json::Value,
}

fn spec_for<T: JsonSchema>(name: &'static str) -> ToolSpec {
    let spec: serde_json::Value = schema_for!(T).to_value();
    let description = spec["description"].as_str().unwrap_or("").to_string(); 
    ToolSpec { name, description, parameters: spec }
} 

pub fn tool_specs() -> Vec<ToolSpec> {
    ToolKind::iter().map(|kind| kind.spec()).collect()
}

pub fn execute(ws:&workspace::Workspace, call: ToolCall) -> Result<ToolOutput, ToolError> {
    match call {
        ToolCall::ReadFile(args) => {
            read_file(ws, args)
        }
        ToolCall::ListDir(args ) => {
            list_dir(ws, args)
        }
        ToolCall::WriteFile(args) => write_file::write_file(ws, args),
    }
}

pub fn execute_json(ws:&workspace::Workspace, call: serde_json::Value) -> Result<ToolOutput, ToolError> {
    let tool_json = serde_json::from_value::<ToolCall>(call)?;
    execute(ws, tool_json)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip_and_specs_exist_for_every_tool() {
        for kind in ToolKind::iter() {
            let name: &'static str = kind.into();
            assert_eq!(ToolKind::from_name(name), Some(kind));
            let spec = kind.spec();
            assert_eq!(spec.name, name);
            assert!(!spec.description.is_empty(), "{name} needs a description");
            assert_eq!(spec.parameters["type"], "object");
        }
        assert_eq!(ToolKind::from_name("rm_rf"), None);
    }

    #[test]
    fn only_state_changing_tools_need_approval() {
        assert!(!ToolKind::ReadFile.needs_approval());
        assert!(!ToolKind::ListDir.needs_approval());
        assert!(ToolKind::WriteFile.needs_approval());
    }
}
