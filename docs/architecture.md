# Architecture

rustopviewer currently has five main layers:

## Host runtime and TUI

- `src/main.rs` starts the app, initializes platform-specific process state, loads config, and launches host/agent/standalone modes plus the local TUI or headless runtime.
- `src/tui.rs` renders the local terminal control surface where users choose monitors, inspect access paths, and manage pairing/session state.

## Fleet registry

- `src/fleet.rs` owns the in-memory device roster used by `rustopviewer host`.
- Agents call `POST /v1/fleet/register` and `POST /v1/fleet/heartbeat` with the shared Masterdale bearer.
- The browser dashboard and `rustopviewer devices` read `GET /v1/fleet/devices`.

## Capture and state

- `src/capture.rs` enumerates monitors and captures frames from the selected display.
- `src/state.rs` stores config, monitor inventory, capture status, fleet role, and the latest encoded frame.
- `src/config.rs` persists local application settings such as the auth token and capture preferences.

## Remote control server

- `src/server.rs` serves the browser UI, authenticated API endpoints, and fleet registry routes.
- `assets/remote.html` contains the current desktop/mobile browser client, including the fleet device list after Masterdale login.
- `src/security.rs` owns the one-time pairing flow, short-lived remote sessions, and remembered-browser trust records.

## Input injection

- `src/input.rs` translates API requests into cross-platform mouse and keyboard events.

## Design Principles

- Keep the trust boundary obvious.
- Prefer small modules with explicit responsibilities.
- Treat remote-input behavior as security-sensitive.
- Make degraded states visible instead of silently failing.
- Keep local-first operation simple, then layer private publishing paths on top.
- Use one shared Masterdale bearer for the private fleet; do not unlock admin with that bearer.
