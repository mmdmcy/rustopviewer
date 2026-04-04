# Architecture

rustopviewer currently has four main layers:

## Native desktop app

- `src/main.rs` starts the app, initializes DPI awareness, loads config, and launches the desktop UI plus background workers.
- `src/app.rs` renders the local control window where users choose monitors and connection URLs.

## Capture and state

- `src/capture.rs` enumerates monitors and captures frames from the selected display.
- `src/state.rs` stores config, monitor inventory, capture status, and the latest encoded frame.
- `src/config.rs` persists local application settings such as the auth token and capture preferences.

## Remote control server

- `src/server.rs` serves the mobile web UI and the authenticated API endpoints.
- `assets/remote.html` contains the current iPhone-oriented browser client.

## Input injection

- `src/input.rs` translates API requests into Windows mouse and keyboard events.

## Design Principles

- Keep the trust boundary obvious.
- Prefer small modules with explicit responsibilities.
- Treat remote-input behavior as security-sensitive.
- Make degraded states visible instead of silently failing.
