# LHA Product Protocol

This private `lha` module defines the protocol types used inside the LHA
product, including internal communication between the product runtime and TUI as
well as external app-server types.

This module should have minimal dependencies and avoid material business logic.
Prefer adding extension traits or adapters in higher-level modules when protocol
types need behavior.
