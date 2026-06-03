# Identities

An identity is a preset that changes how LHA behaves for a session or turn.
When identities are enabled, use `/identity` in the TUI to open the identity
selector.

The active identity may also appear in the footer or status UI so you can see
which mode the next turn will use.

## Built-in Identities

LHA currently includes five built-in identities:

- `nobody`
- `planner`
- `programmer`
- `explorer`
- `reviewer`

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
on product/design tradeoffs. In planner mode, LHA should:

- explore the repository first when local context can answer questions;
- ask clarifying questions for important choices or tradeoffs;
- avoid changing repo-tracked files while still in planner mode;
- produce a final `<proposed_plan>` block when the plan is decision-complete.

Clients that support planner output may show a separate `plan` item for planner
identity turns.

When goals are enabled, the TUI can start implementation from a planner plan by
switching to `programmer` and creating a `/goal` that points at the persisted
proposed plan.

## `programmer`

`programmer` is code/execution mode.

Use `programmer` when you want LHA to implement changes, edit files, run
formatters, run tests, or carry out an already-decided plan.

`programmer` is also the only identity that can run and continue `/goal`
long-running goals. Active goals remain stored if you switch away, but they do
not continue until you switch back to `programmer`.

General repository instructions, sandboxing rules, and approval requirements
still apply in `programmer` mode.

## `explorer`

`explorer` is a read-only codebase navigation identity. It is intended for
isolated one-shot jobs launched by `spawn_agent` or `lha exec --identity
explorer`.

Use `explorer` when LHA needs a narrow repository fact or path-specific answer
but the search process should not enter the main agent context. The runtime
only exposes read-only navigation tools (`grep_files`, `read_file`, and
`list_dir`) for this identity.

`explorer` is a bounded job identity, not a long-lived sub-agent.

## `reviewer`

`reviewer` is a read-only code review identity. `/review` launches it through a
CLI-backed one-shot job, then folds only the final review result back into the
main thread.

Use `reviewer` for automated review findings. The runtime only exposes
read-only navigation tools, read-only inspection command tools, and delegated
explorer-job tools for this identity.

`reviewer` is a bounded job identity, not a long-lived sub-agent.

## Choosing An Identity

| Goal | Recommended identity |
| --- | --- |
| I want a normal assistant response. | `nobody` |
| I want a plan before edits. | `planner` |
| I want code changes now. | `programmer` |
| I want isolated codebase exploration. | `explorer` |
| I want an isolated code review. | `reviewer` |
| I have a large ambiguous task. | Start with `planner`, then switch to `programmer`. |
| I already have a precise task. | `programmer` |

## Switching Identities

Use `/identity` in the TUI to open the identity selector. The command appears
only when identities are enabled.

Switching identities affects subsequent turns. It does not rewrite earlier
turns in the conversation.

When you resume or fork a saved thread, LHA restores the last identity recorded
for that thread. Model selection still follows the current resume/configuration
overrides, so resuming with a different model does not change the restored
identity.

Some TUI footers may show a shortcut such as `Shift+Tab` for changing the active
identity.

## Limitations

- Identities are currently built-in presets.
- `planner` is not an execution mode; it is for producing a decision-complete
  plan.
- `programmer` does not override sandboxing, approval requirements, or
  repository instructions.
- `/goal` requires `programmer`; planner, explorer, reviewer, and nobody turns
  should switch identities before creating or continuing a goal.
- `explorer` and `reviewer` are one-shot, read-only identities. They do not
  support ongoing child-agent conversations. `reviewer` may start bounded
  `explorer` jobs for isolated codebase facts, but not nested reviewer jobs.
- Workflow identities are a separate planned/experimental design and do not
  replace the current built-in identities.

## Related Documentation

- [Slash commands](./slash-commands.md) documents `/identity`.
- [Design Philosophy](./design-philosophy.md) explains LHA's single-agent
  product model and bounded delegation rules.
- [Workflow identities](./workflow-identities.md) describes the planned
  structured workflow identity design.
