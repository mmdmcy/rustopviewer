# Architecture

rustopviewer currently has four main layers:

## Host runtime and TUI

- `src/main.rs` starts the app, initializes platform-specific process state, loads config, and launches the host TUI plus background workers.
- `src/tui.rs` renders the local terminal control surface where users choose monitors, inspect access paths, and manage pairing/session state.

## Capture and state

- `src/capture.rs` enumerates monitors and captures frames from the selected display.
- `src/state.rs` stores config, monitor inventory, capture status, and the latest encoded frame.
- `src/config.rs` persists local application settings such as the auth token and capture preferences.

## Remote control server

- `src/server.rs` serves the browser UI and the authenticated API endpoints.
- `assets/remote.html` contains the current desktop/mobile browser client.

## Input injection

- `src/input.rs` translates API requests into cross-platform mouse and keyboard events.

## Design Principles

- Keep the trust boundary obvious.
- Prefer small modules with explicit responsibilities.
- Treat remote-input behavior as security-sensitive.
- Make degraded states visible instead of silently failing.
