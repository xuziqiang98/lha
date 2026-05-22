# Slash Commands

Slash commands are built-in TUI shortcuts that run local Adam actions without
asking the model to interpret the request.

## Using Slash Commands

- Type `/` at the start of the composer to open the command popup.
- Keep typing to filter the popup, then use Tab or Enter to complete/select a
  command.
- Tab completion can leave a trailing space, for example `/diff `. Pressing
  Enter still runs the bare command.
- Most built-in commands do not accept arguments. Only `/review <instructions>`,
  `/rename <name>`, and `/buddy ...` accept built-in command arguments.
- Commands marked "No" in the "Available During Task" column are disabled while
  an agent task is running.

## Built-In Commands

| Command | Args | Available During Task | Usage |
| --- | --- | --- | --- |
| `/model` | None | No | Open the model and reasoning-effort selector. |
| `/providers` | None | No | Add or configure a model provider and save models for it. |
| `/permissions` | None | No | Open the permissions UI for approval and sandbox settings. |
| `/approvals` | None | No | Open the approval policy selector. This command is hidden from the default popup list. |
| `/setup-elevated-sandbox` | None | No | Start elevated sandbox setup on supported Windows degraded-sandbox sessions. |
| `/experimental` | None | No | Open experimental feature toggles. |
| `/buddy` | Optional buddy subcommand | Yes | Show current TUI buddy details, or run a buddy subcommand. |
| `/skills` | None | Yes | Open the skills modal. |
| `/review` | Optional instructions | No | Review current changes. With args, run a custom review request using the provided instructions. |
| `/rename` | Optional thread name | Yes | Open the rename prompt. With args, rename the current thread directly. |
| `/new` | None | No | Start a new chat session. |
| `/resume` | None | No | Open the saved chat resume picker. |
| `/fork` | None | No | Fork the current chat. |
| `/init` | None | No | Ask Adam to create an `AGENTS.md` instruction file unless one already exists in the current directory. |
| `/compact` | None | No | Summarize the conversation to reduce context usage. |
| `/identity` | None | No | Open the identity selector when identities are enabled. |
| `/changelog` | None | Yes | Show added, modified, and deleted files. |
| `/diff` | None | Yes | Show git diff, including untracked files. |
| `/mention` | None | Yes | Insert `@` to start mentioning a file. |
| `/status` | None | Yes | Show session configuration and token usage. |
| `/plan` | None | Yes | Jump to the latest proposed plan in the transcript. |
| `/bottom` | None | Yes | Scroll the transcript to the bottom. |
| `/mcp` | None | Yes | List configured MCP tools. |
| `/logout` | None | No | Log out of Adam and exit the session. |
| `/exit` | None | Yes | Exit Adam. |
| `/quit` | None | Yes | Alias for exiting Adam. This command is hidden from the default popup list. |
| `/feedback` | None | Yes | Open the local feedback flow when feedback is enabled, otherwise show disabled feedback details. |
| `/ps` | None | Yes | List background terminals. |
| `/stop` | None | Yes | Stop all background terminals. |
| `/clean` | None | Yes | Alias for `/stop`. |
| `/personality` | None | No | Open the communication style selector when the personality command is enabled. |

## Buddy Subcommands

| Command | Usage |
| --- | --- |
| `/buddy` | Show buddy details. |
| `/buddy pet` | Pet the current buddy if it is ready. |
| `/buddy show` | Enable/show the buddy if it is ready. |
| `/buddy hide` | Disable/hide the buddy. |
| `/buddy mute` | Mute buddy speech. |
| `/buddy unmute` | Unmute buddy speech. |
| `/buddy talk on` | Enable observer speech reactions. |
| `/buddy talk off` | Disable observer speech reactions. |

`/buddy hatch` and `/buddy rename` are accepted, but they only show
informational messages because buddy generation and naming are automatic.

## Aliases And Hidden Popup Entries

- `/exit` and `/quit` both exit Adam. `/quit` is hidden from the default popup
  list so the popup shows one entry for the action.
- `/stop` and `/clean` both stop all background terminals.
- `/permissions` and `/approvals` open related permission/approval controls.
  `/approvals` is available by exact command name but hidden from the default
  popup list.

## Feature-Gated Commands

Some commands are visible only when the current session supports them:

- `/identity` appears when identities are enabled.
- `/personality` appears when the personality command is enabled.
- `/setup-elevated-sandbox` appears only for supported Windows degraded-sandbox
  sessions.

## Custom Prompt Commands

Custom prompt commands use the `/prompts:<name>` form. They expand a saved
Markdown prompt into the composer submission.

Adam discovers custom prompts from `$ADAM_HOME/prompts`:

- Only `.md` files are discovered.
- The file stem becomes the prompt name. For example,
  `$ADAM_HOME/prompts/review-api.md` becomes `/prompts:review-api`.
- Prompt names that collide with built-in slash commands are excluded from the
  popup.
- Prompt files can include optional frontmatter keys:
  - `description`: short text shown in the slash popup.
  - `argument-hint` or `argument_hint`: hint text for expected arguments.

Named placeholders use uppercase shell-style names such as `$USER` or
`$BRANCH`. If a prompt contains named placeholders, provide every required value
as `KEY=value`:

```text
/prompts:review USER=Alice BRANCH=main
/prompts:pair USER="Alice Smith" BRANCH=dev-main
```

Values with spaces must be quoted. Tokens that are not `KEY=value` are rejected
for prompts with named placeholders.

Prompts can also use positional placeholders:

- `$1` through `$9` refer to positional arguments.
- `$ARGUMENTS` refers to all remaining positional arguments.

Example:

```text
/prompts:summarize docs/mcp.md
```

## Troubleshooting

- If a command is disabled while a task is running, wait for the task to finish
  or use a command marked available during task.
- If `/diff` says the directory is not a git repository, run Adam from a git
  worktree.
- If `/prompts:<name>` is not listed, verify the file exists under
  `$ADAM_HOME/prompts` and has a `.md` extension.
- If prompt args fail, use `KEY=value` and quote values containing spaces.
