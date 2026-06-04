use crate::product::protocol::workflow::WorkflowDefinition;
use crate::product::protocol::workflow::WorkflowValidationError;
use crate::product::protocol::workflow::WorkflowValidatorDefinition;
use serde_json::Value;
use std::collections::BTreeSet;

pub(crate) fn validate_definition(
    definition: &WorkflowDefinition,
) -> Result<(), Vec<WorkflowValidationError>> {
    let mut errors = Vec::new();
    if definition.id.trim().is_empty() {
        errors.push(error(
            "invalid_workflow_definition",
            "/id",
            "workflow id is required",
        ));
    }
    if definition.steps.is_empty() {
        errors.push(error(
            "invalid_workflow_definition",
            "/steps",
            "workflow requires at least one step",
        ));
    }

    let mut seen = BTreeSet::new();
    for (idx, step) in definition.steps.iter().enumerate() {
        let path = format!("/steps/{idx}");
        if step.id.trim().is_empty() {
            errors.push(error(
                "invalid_workflow_definition",
                format!("{path}/id"),
                "step id is required",
            ));
        }
        if !seen.insert(step.id.clone()) {
            errors.push(error(
                "invalid_workflow_definition",
                format!("{path}/id"),
                format!("duplicate step id `{}`", step.id),
            ));
        }
        for dep in &step.depends_on {
            if !seen.contains(dep) {
                errors.push(error(
                    "invalid_workflow_definition",
                    format!("{path}/depends_on"),
                    format!("dependency `{dep}` must reference an earlier step"),
                ));
            }
        }
        if let Err(err) = jsonschema::validator_for(&step.output_schema) {
            errors.push(error(
                "invalid_workflow_definition",
                format!("{path}/output_schema"),
                format!("invalid JSON schema: {err}"),
            ));
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

pub(crate) fn validate_artifact(
    definition: &WorkflowDefinition,
    step_id: &str,
    artifact: &Value,
    artifacts: &std::collections::BTreeMap<String, super::engine::WorkflowArtifact>,
) -> Vec<WorkflowValidationError> {
    let Some(step) = definition.steps.iter().find(|step| step.id == step_id) else {
        return vec![error(
            "unknown_step",
            "/step_id",
            format!("unknown workflow step `{step_id}`"),
        )];
    };

    let mut errors = Vec::new();
    match jsonschema::validator_for(&step.output_schema) {
        Ok(validator) => {
            for err in validator.iter_errors(artifact) {
                errors.push(error(
                    "schema_validation_failed",
                    err.instance_path.to_string(),
                    err.to_string(),
                ));
            }
        }
        Err(err) => errors.push(error(
            "invalid_workflow_definition",
            "/output_schema",
            format!("invalid JSON schema: {err}"),
        )),
    }

    for validator in &step.validators {
        errors.extend(validate_builtin_validator(validator, artifact, artifacts));
    }

    errors
}

fn validate_builtin_validator(
    validator: &WorkflowValidatorDefinition,
    artifact: &Value,
    artifacts: &std::collections::BTreeMap<String, super::engine::WorkflowArtifact>,
) -> Vec<WorkflowValidationError> {
    match validator {
        WorkflowValidatorDefinition::UniqueIds { path } => {
            let mut seen = BTreeSet::new();
            let mut errors = Vec::new();
            for value in collect_path_values(artifact, path) {
                let Some(id) = value.as_str() else {
                    errors.push(error(
                        "validator_failed",
                        path,
                        "unique_ids matched a non-string value",
                    ));
                    continue;
                };
                if !seen.insert(id.to_string()) {
                    errors.push(error(
                        "validator_failed",
                        path,
                        format!("duplicate id `{id}`"),
                    ));
                }
            }
            errors
        }
        WorkflowValidatorDefinition::ReferencesExistingIds {
            from,
            target,
            target_path,
        } => validate_references(from, target, target_path, artifact, artifacts, false),
        WorkflowValidatorDefinition::ReferencesAllIds {
            from,
            target,
            target_path,
        } => validate_references(from, target, target_path, artifact, artifacts, true),
    }
}

fn validate_references(
    from: &str,
    target: &str,
    target_path: &str,
    artifact: &Value,
    artifacts: &std::collections::BTreeMap<String, super::engine::WorkflowArtifact>,
    require_all: bool,
) -> Vec<WorkflowValidationError> {
    let Some(target_artifact) = artifacts.get(target) else {
        return vec![error(
            "dependency_missing",
            target_path,
            format!("target step `{target}` has no artifact"),
        )];
    };
    let refs = collect_string_set(artifact, from);
    let targets = collect_string_set(&target_artifact.value, target_path);
    let mut errors = Vec::new();

    for id in refs.difference(&targets) {
        errors.push(error(
            "validator_failed",
            from,
            format!("reference `{id}` does not exist in step `{target}`"),
        ));
    }
    if require_all {
        for id in targets.difference(&refs) {
            errors.push(error(
                "validator_failed",
                from,
                format!("id `{id}` from step `{target}` is not referenced"),
            ));
        }
    }
    errors
}

fn collect_string_set(value: &Value, path: &str) -> BTreeSet<String> {
    collect_path_values(value, path)
        .into_iter()
        .filter_map(|value| value.as_str().map(ToString::to_string))
        .collect()
}

fn collect_path_values<'a>(value: &'a Value, path: &str) -> Vec<&'a Value> {
    let parts: Vec<&str> = path
        .trim_start_matches('/')
        .split('/')
        .filter(|part| !part.is_empty())
        .collect();
    let mut current = vec![value];
    for part in parts {
        let mut next = Vec::new();
        for value in current {
            if part == "*" {
                if let Some(array) = value.as_array() {
                    next.extend(array);
                }
            } else if let Some(object) = value.as_object()
                && let Some(child) = object.get(part)
            {
                next.push(child);
            }
        }
        current = next;
    }
    current
}

fn error(
    code: impl Into<String>,
    path: impl Into<String>,
    message: impl Into<String>,
) -> WorkflowValidationError {
    WorkflowValidationError::new(code, path, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::product::protocol::workflow::WorkflowMode;
    use crate::product::protocol::workflow::WorkflowStepDefinition;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    fn workflow(validators: Vec<WorkflowValidatorDefinition>) -> WorkflowDefinition {
        WorkflowDefinition {
            id: "test".to_string(),
            identity_id: "test".to_string(),
            mode: WorkflowMode::Sequential,
            steps: vec![WorkflowStepDefinition {
                id: "one".to_string(),
                label: "One".to_string(),
                prompt: String::new(),
                depends_on: Vec::new(),
                output_schema: json!({
                    "type": "object",
                    "properties": { "items": { "type": "array" } },
                    "required": ["items"],
                    "additionalProperties": false
                }),
                allowed_tools: None,
                validators,
            }],
        }
    }

    #[test]
    fn schema_validation_reports_errors() {
        let definition = workflow(Vec::new());
        let errors = validate_artifact(
            &definition,
            "one",
            &json!({ "items": [], "extra": true }),
            &Default::default(),
        );

        assert_eq!(errors[0].code, "schema_validation_failed");
    }

    #[test]
    fn unique_ids_reports_duplicates() {
        let definition = workflow(vec![WorkflowValidatorDefinition::UniqueIds {
            path: "/items/*/id".to_string(),
        }]);
        let errors = validate_artifact(
            &definition,
            "one",
            &json!({ "items": [{ "id": "a" }, { "id": "a" }] }),
            &Default::default(),
        );

        assert_eq!(errors[0].code, "validator_failed");
    }
}
