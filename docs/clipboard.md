# Clipboard Copying

LHA uses two different clipboard paths for text copied from the TUI.

On a local Linux desktop, LHA first tries the system clipboard through the
desktop session. If that fails, it falls back to OSC52.

In remote terminals such as SSH or VS Code Remote, LHA first tries OSC52. OSC52
is an escape sequence sent to the terminal emulator so the local terminal can
write its clipboard. This is what allows a process running on a remote Linux
host to copy into the client machine's clipboard.

## tmux

tmux can forward OSC52 in two common ways:

- `bare`: LHA sends a normal OSC52 sequence. tmux forwards it when
  `set-clipboard on` is enabled and the terminal has the `Ms` capability.
- `passthrough`: LHA wraps OSC52 in tmux passthrough escape sequences.

Configure the mode in `~/.lha/config.toml`:

```toml
[tui]
osc52_tmux_mode = "auto"
```

Supported values are:

- `auto`: LHA's default strategy. In tmux, this currently uses `bare`.
- `bare`: Send bare OSC52 inside tmux.
- `passthrough`: Send tmux passthrough-wrapped OSC52 inside tmux.

For Windows Termius connecting to Ubuntu over SSH and running LHA inside tmux,
use:

```toml
[tui]
osc52_tmux_mode = "bare"
```

## Diagnostics

To test whether your terminal accepts bare OSC52, run this and paste into a
local text field on the client machine:

```bash
printf '\033]52;c;YmFyZQo=\a'
```

The pasted text should be:

```text
bare
```

To test tmux passthrough, run this inside tmux and paste into a local text field
on the client machine:

```bash
printf '\033Ptmux;\033\033]52;c;cGFzc3Rocm91Z2gK\a\033\\'
```

The pasted text should be:

```text
passthrough
```

On a remote Linux desktop, tools such as `wl-copy` and `wl-paste` only prove that
the remote Ubuntu clipboard works. They do not prove that OSC52 can write the
client machine's clipboard.

Useful tmux checks:

```bash
tmux show -s set-clipboard
tmux info | grep 'Ms:'
```
