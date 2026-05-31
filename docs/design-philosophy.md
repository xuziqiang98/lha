# Design Philosophy

LHA's product model is a single main agent. The user works with one LHA, and
that main agent owns the task narrative, user interaction, planning, execution,
approval flow, and final answer.

## Single-Agent Product Model

LHA should not present model-visible multi-agent collaboration as the core
experience. The main agent is the only long-lived controller. It decides what
work is needed, asks the user for input when required, and integrates final
results into the active thread.

This applies to both UI and runtime design:

- User-facing concepts should describe one LHA working in one thread.
- Runtime APIs should avoid long-lived child-agent sessions.
- Prompt and tool names should avoid implying a team of persistent agents.
- Final user-visible results should be folded back through the main agent.

## Bounded Delegation

Some tasks benefit from isolated context. Exploration and review can involve
noisy search paths, large intermediate outputs, or narrow checks that should not
fill the main thread. LHA supports those cases with bounded one-shot delegated
jobs.

Delegated jobs are not conversational agents:

- `explorer` and `reviewer` are identities/job types.
- A delegated job runs in a separate `lha exec --identity ...` process.
- The job receives one task, produces one final result, and exits.
- The main agent consumes only the final artifact or result.

The runtime may expose tools such as `spawn_agent` for compatibility with model
expectations, but those tools start one-shot jobs. They do not create long-lived
sub-agents, chat sessions, or independent controllers.

## Context Is An Implementation Detail

Finite context should not define the product model. LHA's target experience is
bounded context that feels like effectively unbounded working memory. Users
should not need to manage a swarm of agents to work around context limits.

Context isolation is still useful. It keeps noisy exploration, targeted review,
and other bounded work out of the main thread until there is a result worth
preserving. That isolation is an execution strategy, not a reason to expose a
multi-agent product story.

## Review And Exploration

`/review` uses the same bounded delegation model as isolated exploration. The
runtime starts the top-level reviewer job, waits for the final review result,
parses it, and emits `ExitedReviewMode` in the main thread. A reviewer job may
start bounded `explorer` jobs for codebase facts, but nested reviewer jobs are
unsupported.

Exploration jobs should answer narrow repository questions. They may run in
parallel when the questions are independent, but each job still has one prompt,
one result, and one lifetime.

## Non-Goals

LHA does not support these as part of the delegated job model:

- Long-lived child agent sessions.
- Multi-turn chat with child agents.
- `send_input` for delegated jobs.
- `resume_agent` for delegated jobs.
- `fork_context` for delegated jobs.
- Batch worker-agent contracts.
- A required `lha exec --json` control protocol for delegated job results.

## Naming Rules

Names should match the single-agent model:

- Use `job` for one-shot delegated process work.
- Use `identity` for behavior presets such as `explorer` and `reviewer`.
- Use `turn` or `task` for work owned by the main session.
- Use `attach` for UI helpers that connect an existing thread to a surface.

Avoid names such as `subagent`, `collab`, or hidden review `thread` when the
implementation is a bounded delegated job.
