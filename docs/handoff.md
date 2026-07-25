# RustOpViewer Handoff

Last updated: 2026-07-25

This file is the editable working handoff for future sessions.

## Current Position

- Optional auth layers on top of pairing/password:
  1. Masterdale bearer (`ROV_MASTERDALE_TOKEN`, or process `DALE_TOKEN`) for
     persistent view/control from the remote page.
  2. Optional LinuxMice OIDC that issues a normal bounded local session.
- Capture sleeps while no authenticated viewer is active and wakes on the next
  authorized request.
- Media transport keys (play/pause, mute, volume) are on the remote toolbar for
  media-box use.
- Example user unit `packaging/systemd/rustopviewer-masterdale.service` loads
  `%h/.config/rustopviewer/masterdale.env` so operators can point at a shared
  Masterdale token env without hard-coding a checkout path.

## Important UX Decision

Host env holding `DALE_TOKEN` does **not** log a phone browser in by itself.
Browsers must still prove access:

- paste the Masterdale bearer once per origin (stored as
  `masterdale.dashboard.token` in that origin's localStorage), or
- use LinuxMice OIDC (preferred when avoiding bearer paste).

Masterdale dashboard and RustOpViewer do not share localStorage across
different origins even when the storage key name matches.

## Recent Changes

- `src/oidc.rs`, wiring in `main.rs` / `server.rs` / `state.rs` / `security.rs`
- Remote UI Masterdale + LinuxMice access panels in `assets/remote.html`
- Env/docs/packaging for shared Masterdale token + optional OIDC

## How To Extend Safely

- Admin API stays loopback + `ROV_ADMIN_TOKEN`; Masterdale bearer must not unlock
  admin.
- Keep host pointer/keyboard permission switches authoritative even when
  Masterdale or OIDC authorizes the viewer.
- Do not commit real tokens, private hostnames, or subject UUIDs.

## Verification Baseline

```sh
cargo test
cargo clippy --all-targets -- -D warnings
git diff --check
```

Verified 2026-07-25: 33 tests passed, strict Clippy passed, and
`git diff --check` was clean. Private-tailnet browser-shaped verification also
confirmed that the HTTPS relying-party route reaches LinuxMice authorization
with S256 PKCE, state, nonce, and the registered RustOpViewer callback.

## Known Loose Ends

- Live private-tailnet dogfood is installed, but this is not an OIDC
  conformance result, production identity claim, or independent security
  review.
