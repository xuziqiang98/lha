use adam_protocol::config_types::IdentityCapabilities;
use adam_protocol::config_types::IdentityKind;
use adam_protocol::config_types::IdentityMask;
use adam_protocol::openai_models::ReasoningEffort;

const PLANNER_PROMPT: &str = include_str!("../planner/prompt.md");
const PROGRAMMER_PROMPT: &str = include_str!("../programmer/prompt.md");
const EXPLORER_PROMPT: &str = include_str!("../explorer/prompt.md");
const REVIEWER_PROMPT: &str = include_str!("../reviewer/prompt.md");

pub fn builtin_identity_presets() -> Vec<IdentityMask> {
    vec![
        nobody_preset(),
        planner_preset(),
        programmer_preset(),
        explorer_preset(),
        reviewer_preset(),
    ]
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

pub fn explorer_preset() -> IdentityMask {
    IdentityMask {
        name: "explorer".to_string(),
        kind: Some(IdentityKind::Explorer),
        model: None,
        reasoning_effort: Some(Some(ReasoningEffort::Low)),
        developer_instructions: Some(Some(EXPLORER_PROMPT.to_string())),
        capabilities: IdentityCapabilities { write_tools: false },
    }
}

pub fn reviewer_preset() -> IdentityMask {
    IdentityMask {
        name: "reviewer".to_string(),
        kind: Some(IdentityKind::Reviewer),
        model: None,
        reasoning_effort: Some(Some(ReasoningEffort::Medium)),
        developer_instructions: Some(Some(REVIEWER_PROMPT.to_string())),
        capabilities: IdentityCapabilities { write_tools: false },
    }
}
