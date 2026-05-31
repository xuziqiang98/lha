# Workflow Identities

Workflow identities are a planned extension point for identities that need more
structure than a prompt can reliably provide. They are intended for identities
such as `architect`, where the agent must complete a fixed sequence of steps,
produce a validated artifact for each step, and use earlier artifacts as inputs
to later steps.

The built-in `planner` and `programmer` identities are intentionally unchanged.
This mechanism is for future stricter identities.

## Goals

- Define workflows declaratively from identity configuration.
- Enforce a sequential state machine in runtime code.
- Require each step to submit a JSON artifact through a dedicated tool.
- Validate artifacts with JSON Schema before advancing the workflow.
- Allow built-in cross-artifact validators for common id/reference checks.
- Persist workflow progress and artifacts to rollout history for resume, fork,
  audit, and debugging.
- Allow each step to narrow the tool set available to the model.

## Non-goals

- Replacing the existing `planner` or `programmer` implementations.
- Running arbitrary validator code from identity configuration.
- Supporting external scripts, WASM validators, or dynamic Rust plugins in the
  first implementation.
- Automatically running multiple model turns without user or model interaction.
- Building a full TUI workflow visualization in the first implementation.

## Identity Manifest Shape

A workflow identity is described by an identity manifest plus prompt and schema
files stored next to it.

```text
src/agent/identity/architect/
  identity.toml
  prompt.md
  steps/
    requirements.md
    architecture.md
    risk_review.md
    implementation_plan.md
  schemas/
    requirements.schema.json
    architecture.schema.json
    risk_review.schema.json
    implementation_plan.schema.json
```

Example manifest:

```toml
id = "architect"
label = "architect"
prompt = "prompt.md"

[capabilities]
write_tools = false

[workflow]
id = "architect_v1"
mode = "sequential"
artifact_store = "rollout"

[[workflow.steps]]
id = "requirements"
label = "Requirements"
prompt = "steps/requirements.md"
output_schema = "schemas/requirements.schema.json"
allowed_tools = ["request_user_input", "read_file", "grep_files"]

[[workflow.steps]]
id = "architecture"
label = "Architecture"
depends_on = ["requirements"]
prompt = "steps/architecture.md"
output_schema = "schemas/architecture.schema.json"
allowed_tools = ["read_file", "grep_files"]
validators = [
  { type = "references_existing_ids", from = "/requirements/*/id", target = "requirements", target_path = "/requirements/*/id" },
]
```

Manifest rules:

- `id` values are stable machine identifiers and should match
  `^[a-z][a-z0-9_-]*$`.
- The first version only supports `workflow.mode = "sequential"`.
- Step ids must be unique within a workflow.
- `depends_on` may only reference earlier steps.
- A missing `depends_on` means "depend on the previous step" except for the
  first step, which has no dependency.
- Paths such as `prompt` and `output_schema` are resolved relative to the
  manifest directory and must not escape that directory.
- `allowed_tools` can only narrow the tools already enabled by runtime features.

## Runtime State Machine

The runtime owns the workflow state. The model can request state transitions only
by calling `workflow_submit_artifact`.

States:

- `not_started`: the workflow exists but has not entered the first step.
- `in_progress`: a current step is active.
- `completed`: all steps have accepted artifacts.
- `failed`: persisted state is inconsistent with the workflow definition.

Transition rules:

- Starting a workflow sets the current step to the first step.
- Only the current step can accept an artifact.
- A step cannot complete until all dependencies have accepted artifacts.
- A submitted artifact must pass JSON Schema validation.
- A submitted artifact must pass all built-in validators for the step.
- Failed validation does not advance state.
- A successful submission stores the artifact, marks the step completed, and
  advances to the next step.
- The final successful submission marks the workflow completed.

## Artifact Submission Tool

Strict workflow progress is driven by a dedicated function tool:

```text
workflow_submit_artifact
```

Input schema:

```json
{
  "type": "object",
  "properties": {
    "step_id": { "type": "string" },
    "artifact": { "type": "object" }
  },
  "required": ["step_id", "artifact"],
  "additionalProperties": false
}
```

Successful output:

```json
{
  "status": "accepted",
  "workflow_id": "architect_v1",
  "completed_step": "requirements",
  "next_step": "architecture",
  "workflow_completed": false
}
```

Rejected output:

```json
{
  "status": "rejected",
  "workflow_id": "architect_v1",
  "step_id": "architecture",
  "errors": [
    {
      "code": "schema_validation_failed",
      "path": "/components/0/id",
      "message": "expected string"
    }
  ]
}
```

Assistant text is never considered a valid artifact. Only a successful tool call
advances the workflow.

## Artifact Validation

Each step has a JSON Schema. The runtime compiles and validates schemas in code,
not by prompting the model.

The first implementation should use a mature JSON Schema validator crate such as
`jsonschema`. Schemas with remote URI references should be rejected in v1 so
workflow execution cannot fetch remote schema content.

Validation errors are returned to the model as structured errors with:

- `code`
- `path`
- `message`

The first version also supports a small set of built-in validators:

- `unique_ids`: all ids collected from a path in the current artifact must be
  unique.
- `references_existing_ids`: ids collected from the current artifact must exist
  in a previous step artifact.
- `references_all_ids`: ids collected from a previous step artifact must all be
  referenced by the current artifact.

Validator paths use a restricted JSON Pointer glob syntax, for example:

```text
/requirements/*/id
/components/*/requirement_ids/*
```

Only object keys and array wildcards are supported in v1.

## Rollout Persistence

Workflow progress is persisted as first-class rollout items. This allows the
runtime to rebuild workflow state when a thread is resumed or forked.

Persisted events include:

- workflow started
- artifact submitted
- step completed
- validation failed
- workflow completed

Artifacts are stored as JSON values in rollout history. Each artifact also stores
a SHA-256 hash for diagnostics and stable references in UI or logs.

Because workflow items extend the rollout item set, adding the persisted item
variant should bump the rollout schema version.

## Tool Filtering

Each workflow step may declare `allowed_tools`. If present, the runtime filters
the tool specs for that step to this allowlist plus `workflow_submit_artifact`.

The filter is only a narrowing layer:

- It cannot enable a tool disabled by feature flags.
- It cannot bypass model/provider tool restrictions.
- It cannot bypass approval or sandbox policies.

## Prompt Context Injection

For active workflow steps, runtime injects a developer instruction fragment that
contains:

- workflow id
- current step id and label
- current step prompt
- completed artifacts needed by the step
- a requirement to submit the artifact through `workflow_submit_artifact`

This context is separate from user messages and base instructions.

## Implementation Location

The first implementation belongs in the LHA product runtime:

- Protocol and rollout data types: `src/core/protocol`
- Workflow engine and validation: `src/agent/runtime/src/workflow`
- Tool handler: `src/agent/runtime/src/tools/handlers/workflow.rs`
- Tool filtering: `src/agent/runtime/src/tools/spec.rs`
- Session wiring: `src/agent/runtime/src/codex.rs` and session state

It should not be moved into `lha-agent-runtime` until the product-specific
policy surface is better understood.

## Testing Expectations

Tests should cover:

- invalid workflow definitions
- schema validation failures
- validator failures
- successful sequential advancement
- rejected attempts to skip steps
- rejected attempts to resubmit completed steps
- rollout persistence and state reconstruction
- tool exposure per step
- unchanged behavior for built-in `planner` and `programmer`

