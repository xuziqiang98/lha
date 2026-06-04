pub mod apply_patch;
pub(crate) mod delegated_jobs;
mod dynamic;
mod goal;
mod grep_files;
mod imagegen;
mod list_dir;
mod mcp;
mod mcp_resource;
mod memories;
mod plan;
mod read_file;
mod request_user_input;
mod shell;
mod test_sync;
mod unified_exec;
mod view_image;
mod workflow;

pub use plan::PLAN_TOOL;
use serde::Deserialize;

use crate::product::agent::function_tool::FunctionCallError;
pub use apply_patch::ApplyPatchHandler;
pub use delegated_jobs::DelegatedJobHandler;
pub use dynamic::DynamicToolHandler;
pub use goal::GoalHandler;
pub use grep_files::GrepFilesHandler;
pub use imagegen::ImagegenHandler;
pub use list_dir::ListDirHandler;
pub use mcp::McpHandler;
pub use mcp_resource::McpResourceHandler;
pub use memories::MemoriesHandler;
pub use plan::PlanHandler;
pub(crate) use plan::UPDATE_PLAN_SUCCESS_OUTPUT;
pub use read_file::ReadFileHandler;
pub use request_user_input::RequestUserInputHandler;
pub use shell::ShellCommandHandler;
pub use shell::ShellHandler;
pub use test_sync::TestSyncHandler;
pub use unified_exec::UnifiedExecHandler;
pub use view_image::ViewImageHandler;
pub use workflow::WORKFLOW_SUBMIT_ARTIFACT_TOOL;
pub use workflow::WorkflowHandler;

fn parse_arguments<T>(arguments: &str) -> Result<T, FunctionCallError>
where
    T: for<'de> Deserialize<'de>,
{
    serde_json::from_str(arguments).map_err(|err| {
        FunctionCallError::RespondToModel(format!("failed to parse function arguments: {err}"))
    })
}
