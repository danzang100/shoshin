use std::fmt;
use schemars::JsonSchema;
use schemars::schema_for;
use serde::Deserialize;
use serde::Serialize;
use strum::IntoEnumIterator;
use strum::IntoStaticStr;
use strum::{EnumIter};

use crate::tools::list_dir::ListDirArgs;
use crate::tools::read_file;
use crate::tools::list_dir;
use crate::tools::read_file::ReadFileArgs;
use crate::tools::workspace;
use crate::tools::{ToolError, ToolOutput};


#[derive(Deserialize, Debug)]
#[serde(tag = "name", content = "arguments", rename_all = "snake_case")]
pub enum ToolCall {
    ReadFile(read_file::ReadFileArgs),
    ListDir(list_dir::ListDirArgs)
}

#[derive(Debug, Clone, Copy, PartialEq, EnumIter, IntoStaticStr)]
#[strum(serialize_all="snake_case")]
pub enum ToolKind {
    ReadFile,
    ListDir
}

impl ToolKind {
    fn spec(&self) -> ToolSpec{
        match self {
            ToolKind::ReadFile => { 
                spec_for::<ReadFileArgs>(self.into())
            }
            ToolKind::ListDir => {
                spec_for::<ListDirArgs>(self.into())
            }
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
    }
}

pub fn execute_json(ws:&workspace::Workspace, call: serde_json::Value) -> Result<ToolOutput, ToolError> {
    let tool_json = serde_json::from_value::<ToolCall>(call)?;
    execute(ws, tool_json)
}
