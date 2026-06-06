# Linux Sandbox Helper

This private `lha` module contains the Linux sandbox helper implementation.
The installed package still exposes a single `lha` binary; when that binary is
invoked through an arg0 alias named `lha-linux-sandbox`, it dispatches here and
runs as the sandbox helper.
