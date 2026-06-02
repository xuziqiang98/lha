use serde::Deserialize;
use serde::Serialize;
use serde_json::json;
use std::path::Path;

const STAGE_ONE_INPUT_TEMPLATE: &str = include_str!("../templates/memories/stage_one_input.md");
const CONSOLIDATION_TEMPLATE: &str = include_str!("../templates/memories/consolidation.md");

pub const STAGE_ONE_SYSTEM_PROMPT: &str = include_str!("../templates/memories/stage_one_system.md");

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct StageOneOutput {
    pub raw_memory: String,
    pub rollout_summary: String,
    pub rollout_slug: Option<String>,
}

pub fn build_stage_one_input_message(
    rollout_path: &Path,
    rollout_cwd: &Path,
    rollout_contents: &str,
) -> String {
    STAGE_ONE_INPUT_TEMPLATE
        .replace("{{ rollout_path }}", &rollout_path.display().to_string())
        .replace("{{ rollout_cwd }}", &rollout_cwd.display().to_string())
        .replace("{{ rollout_contents }}", rollout_contents)
}

pub fn build_consolidation_prompt(memory_root: &Path) -> String {
    CONSOLIDATION_TEMPLATE
        .replace("{{ memory_root }}", &memory_root.display().to_string())
        .replace(
            "{{ memory_extensions_root }}",
            &memory_root.join("extensions").display().to_string(),
        )
        .replace(
            "{{ memory_extensions_folder_structure }}",
            "Memory extensions live under extensions/<name>/instructions.md when present.",
        )
        .replace(
            "{{ memory_extensions_primary_inputs }}",
            "Optional source-specific inputs live under extensions/<name>/instructions.md when present.",
        )
        .replace(
            "{{ phase2_workspace_diff_file }}",
            &memory_root.join("phase2_workspace_diff.md").display().to_string(),
        )
}

pub fn stage_one_output_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "raw_memory": { "type": "string" },
            "rollout_summary": { "type": "string" },
            "rollout_slug": { "type": ["string", "null"] }
        },
        "required": ["raw_memory", "rollout_summary", "rollout_slug"]
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    #[test]
    fn stage_one_output_schema_requires_all_fields() {
        assert_eq!(
            stage_one_output_schema(),
            json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "raw_memory": { "type": "string" },
                    "rollout_summary": { "type": "string" },
                    "rollout_slug": { "type": ["string", "null"] }
                },
                "required": ["raw_memory", "rollout_summary", "rollout_slug"]
            })
        );
    }

    #[test]
    fn renders_stage_one_input_with_rollout_context() {
        let rendered = build_stage_one_input_message(
            Path::new("/tmp/rollout.jsonl"),
            Path::new("/tmp/project"),
            "[{\"role\":\"user\"}]",
        );

        assert!(rendered.contains("/tmp/rollout.jsonl"));
        assert!(rendered.contains("/tmp/project"));
        assert!(rendered.contains("[{\"role\":\"user\"}]"));
    }
}
