use adam_protocol::config_types::IdentityCapabilities;
use adam_protocol::config_types::IdentityKind;
use adam_protocol::config_types::IdentityMask;
use adam_protocol::openai_models::ReasoningEffort;

const PLANNER_PROMPT: &str = include_str!("../planner/prompt.md");
const PROGRAMMER_PROMPT: &str = include_str!("../programmer/prompt.md");

pub fn builtin_identity_presets() -> Vec<IdentityMask> {
    vec![nobody_preset(), planner_preset(), programmer_preset()]
}

pub fn nobody_preset() -> IdentityMask {
    IdentityMask {
        name: "nobody".to_string(),
        kind: Some(IdentityKind::Nobody),
        model: None,
        reasoning_effort: None,
        developer_instructions: Some(None),
        capabilities: IdentityCapabilities { write_tools: false },
    }
}

pub fn planner_preset() -> IdentityMask {
    IdentityMask {
        name: "planner".to_string(),
        kind: Some(IdentityKind::Planner),
        model: None,
        reasoning_effort: Some(Some(ReasoningEffort::Medium)),
        developer_instructions: Some(Some(PLANNER_PROMPT.to_string())),
        capabilities: IdentityCapabilities { write_tools: true },
    }
}

pub fn programmer_preset() -> IdentityMask {
    IdentityMask {
        name: "programmer".to_string(),
        kind: Some(IdentityKind::Programmer),
        model: None,
        reasoning_effort: None,
        developer_instructions: Some(Some(PROGRAMMER_PROMPT.to_string())),
        capabilities: IdentityCapabilities { write_tools: true },
    }
}
