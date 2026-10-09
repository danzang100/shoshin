use lightning::tools::read_file::ReadFileArgs;
use lightning::tools::list_dir::ListDirArgs;
use lightning::tools::registry::{self, ToolCall, ToolKind};
use inquire::{error::InquireError, Select, Text};
use lightning::tools::workspace::Workspace;
use strum::{IntoEnumIterator};
use std::path::Path;

fn collect_args(kind:ToolKind) -> Result<ToolCall, InquireError> {
    match kind {
        ToolKind::ReadFile => {
            let file_path = Text::new("Enter file path").prompt()?;
            Ok(ToolCall::ReadFile(ReadFileArgs { file_path }))
        }

        ToolKind::ListDir => {
            let dir_path = Text::new("Enter folder path").prompt()?;
            Ok(ToolCall::ListDir(ListDirArgs { dir_path }))
        }
    }
}


async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let ws = Workspace::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("test-folder"))?;
    let tool_options: Vec<ToolKind> = ToolKind::iter().collect(); 
    let tool_selected: ToolKind = Select::new("Choose the tool for use", tool_options).prompt()?;
    let tool_call = collect_args(tool_selected)?;
    let tool_output = registry::execute(&ws, tool_call)?;
    println!("{}", tool_output.content);
    println!("{}", tool_output.metadata);
    Ok(())
}

#[tokio::main]
async fn main() {
    match run().await {
        Ok(()) => {}
        Err(err) => {
            eprintln!("{}", err)
        }
    }
}
