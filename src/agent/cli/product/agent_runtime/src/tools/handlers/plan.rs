use crate::product::agent::codex::Session;
use crate::product::agent::codex::TurnContext;
use crate::product::agent::function_tool::FunctionCallError;
use crate::product::agent::tools::context::ToolInvocation;
use crate::product::agent::tools::context::ToolOutput;
use crate::product::agent::tools::context::ToolPayload;
use crate::product::agent::tools::registry::ToolHandler;
use crate::product::agent::tools::registry::ToolKind;
use crate::product::agent::tools::spec::JsonSchema;
use crate::product::protocol::config_types::IdentityKind;
use crate::product::protocol::plan_tool::UpdatePlanArgs;
use crate::product::protocol::protocol::EventMsg;
use async_trait::async_trait;
use lha_llm::FunctionToolDescriptor;
use lha_llm::ToolDescriptor;
use std::collections::BTreeMap;
use std::sync::LazyLock;

pub struct PlanHandler;
pub(crate) const UPDATE_PLAN_SUCCESS_OUTPUT: &str = "Plan updated";

pub static PLAN_TOOL: LazyLock<ToolDescriptor> = LazyLock::new(|| {
    let mut plan_item_props = BTreeMap::new();
    plan_item_props.insert(
        "step".to_string(),
        JsonSchema::String {
            description: None,
            enum_values: None,
        },
    );
    plan_item_props.insert(
        "status".to_string(),
        JsonSchema::String {
            description: Some("One of: pending, in_progress, completed".to_string()),
            enum_values: None,
        },
    );

    let plan_items_schema = JsonSchema::Array {
        description: Some("The list of steps".to_string()),
        items: Box::new(JsonSchema::Object {
            properties: plan_item_props,
            required: Some(vec!["step".to_string(), "status".to_string()]),
            additional_properties: Some(false.into()),
        }),
    };

    let mut properties = BTreeMap::new();
    properties.insert(
        "explanation".to_string(),
        JsonSchema::String {
            description: None,
            enum_values: None,
        },
    );
    properties.insert("plan".to_string(), plan_items_schema);

    ToolDescriptor::Function(FunctionToolDescriptor {
        name: "update_plan".to_string(),
        description: r#"Updates the task plan.
Provide an optional explanation and a list of plan items, each with a step and status.
At most one step can be in_progress at a time.
"#
        .to_string(),
        strict: false,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec!["plan".to_string()]),
            additional_properties: Some(false.into()),
        },
    })
});

#[async_trait]
impl ToolHandler for PlanHandler {
    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<ToolOutput, FunctionCallError> {
        let ToolInvocation {
            session,
            turn,
            call_id,
            payload,
            ..
        } = invocation;

        let arguments = match payload {
            ToolPayload::Function { arguments } => arguments,
            _ => {
                return Err(FunctionCallError::RespondToModel(
                    "update_plan handler received unsupported payload".to_string(),
                ));
            }
        };

        let content =
            handle_update_plan(session.as_ref(), turn.as_ref(), arguments, call_id).await?;

        Ok(ToolOutput::Function {
            content,
            content_items: None,
            success: Some(true),
        })
    }
}

/// This function doesn't do anything useful. However, it gives the model a structured way to record its plan that clients can read and render.
/// So it's the _inputs_ to this function that are useful to clients, not the outputs and neither are actually useful for the model other
/// than forcing it to come up and document a plan (TBD how that affects performance).
pub(crate) async fn handle_update_plan(
    session: &Session,
    turn_context: &TurnContext,
    arguments: String,
    _call_id: String,
) -> Result<String, FunctionCallError> {
    if turn_context.identity.kind == IdentityKind::Planner {
        return Err(FunctionCallError::RespondToModel(
            "update_plan is a TODO/checklist tool and is not allowed for the planner identity"
                .to_string(),
        ));
    }
    let args = parse_update_plan_arguments(&arguments)?;
    session
        .send_event(turn_context, EventMsg::PlanUpdate(args))
        .await;
    Ok(UPDATE_PLAN_SUCCESS_OUTPUT.to_string())
}

fn parse_update_plan_arguments(arguments: &str) -> Result<UpdatePlanArgs, FunctionCallError> {
    serde_json::from_str::<UpdatePlanArgs>(arguments).map_err(|e| {
        FunctionCallError::RespondToModel(format!("failed to parse function arguments: {e}"))
    })
}
