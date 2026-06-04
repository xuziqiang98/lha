use async_trait::async_trait;
use serde::Deserialize;
use tokio::fs;

use crate::product::agent::function_tool::FunctionCallError;
use crate::product::agent::protocol::EventMsg;
use crate::product::agent::protocol::ViewImageToolCallEvent;
use crate::product::agent::tools::context::ToolInvocation;
use crate::product::agent::tools::context::ToolOutput;
use crate::product::agent::tools::context::ToolPayload;
use crate::product::agent::tools::handlers::parse_arguments;
use crate::product::agent::tools::registry::ToolHandler;
use crate::product::agent::tools::registry::ToolKind;
use crate::product::protocol::models::ContentItem;
use crate::product::protocol::models::TranscriptItem;
use crate::product::protocol::models::local_image_content_items_with_label_number;

pub struct ViewImageHandler;

#[derive(Deserialize)]
struct ViewImageArgs {
    path: String,
}

#[async_trait]
impl ToolHandler for ViewImageHandler {
    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<ToolOutput, FunctionCallError> {
        let ToolInvocation {
            session,
            turn,
            payload,
            call_id,
            ..
        } = invocation;

        let arguments = match payload {
            ToolPayload::Function { arguments } => arguments,
            _ => {
                return Err(FunctionCallError::RespondToModel(
                    "view_image handler received unsupported payload".to_string(),
                ));
            }
        };

        let args: ViewImageArgs = parse_arguments(&arguments)?;

        let abs_path = turn.resolve_path(Some(args.path));

        let metadata = fs::metadata(&abs_path).await.map_err(|error| {
            FunctionCallError::RespondToModel(format!(
                "unable to locate image at `{}`: {error}",
                abs_path.display()
            ))
        })?;

        if !metadata.is_file() {
            return Err(FunctionCallError::RespondToModel(format!(
                "image path `{}` is not a file",
                abs_path.display()
            )));
        }
        let event_path = abs_path.clone();

        let content: Vec<ContentItem> =
            local_image_content_items_with_label_number(&abs_path, None);
        let input = TranscriptItem::Message {
            id: None,
            role: "user".to_string(),
            content,
            end_turn: None,
        };

        session
            .inject_transcript_items(vec![input])
            .await
            .map_err(|_| {
                FunctionCallError::RespondToModel(
                    "unable to attach image (no active task)".to_string(),
                )
            })?;

        session
            .send_event(
                turn.as_ref(),
                EventMsg::ViewImageToolCall(ViewImageToolCallEvent {
                    call_id,
                    path: event_path,
                }),
            )
            .await;

        Ok(ToolOutput::Function {
            content: "attached local image path".to_string(),
            content_items: None,
            success: Some(true),
        })
    }
}
