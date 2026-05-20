# Identities

An identity is a preset that changes how Adam behaves for a session or turn.
When identities are enabled, use `/identity` in the TUI to open the identity
selector.

The active identity may also appear in the footer or status UI so you can see
which mode the next turn will use.

## Built-in Identities

Adam currently includes three built-in identities:

- `nobody`
- `planner`
- `programmer`

## `nobody`

`nobody` is the minimal/default identity. It does not add an extra
identity-specific prompt on top of the normal session instructions.

Use `nobody` when you want ordinary assistant behavior, general exploration, or
a response that is not shaped by planner or code-mode behavior.

`nobody` does not add the planner workflow constraints described below.

## `planner`

`planner` is for turning a request into a detailed implementation plan before
coding starts.

Use `planner` when a task is ambiguous, large, risky, cross-cutting, or depends
on product/design tradeoffs. In planner mode, Adam should:

- explore the repository first when local context can answer questions;
- ask clarifying questions for important choices or tradeoffs;
- avoid changing repo-tracked files while still in planner mode;
- produce a final `<proposed_plan>` block when the plan is decision-complete.

Clients that support planner output may show a separate `plan` item for planner
identity turns.

## `programmer`

`programmer` is code/execution mode.

Use `programmer` when you want Adam to implement changes, edit files, run
formatters, run tests, or carry out an already-decided plan.

General repository instructions, sandboxing rules, and approval requirements
still apply in `programmer` mode.

## Choosing An Identity

| Goal | Recommended identity |
| --- | --- |
| I want a normal assistant response. | `nobody` |
| I want a plan before edits. | `planner` |
| I want code changes now. | `programmer` |
| I have a large ambiguous task. | Start with `planner`, then switch to `programmer`. |
| I already have a precise task. | `programmer` |

## Switching Identities

Use `/identity` in the TUI to open the identity selector. The command appears
only when identities are enabled.

Switching identities affects subsequent turns. It does not rewrite earlier
turns in the conversation.

Some TUI footers may show a shortcut such as `Shift+Tab` for changing the active
identity.

## Limitations

- Identities are currently built-in presets.
- `planner` is not an execution mode; it is for producing a decision-complete
  plan.
- `programmer` does not override sandboxing, approval requirements, or
  repository instructions.
- Workflow identities are a separate planned/experimental design and do not
  replace the current built-in identities.

## Related Documentation

- [Slash commands](./slash-commands.md) documents `/identity`.
- [Workflow identities](./workflow-identities.md) describes the planned
  structured workflow identity design.
