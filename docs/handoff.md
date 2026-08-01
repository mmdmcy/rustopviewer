# RustOpViewer Handoff

Last updated: 2026-08-02

## Current Position

- Fleet host/agent modes with shared Masterdale bearer (`DALE_TOKEN` / `ROV_MASTERDALE_TOKEN`).
- Lawnmower runs `rustopviewer host`; agents register via `/v1/fleet/*`.
- Browser dashboard loads the live fleet roster after Masterdale login.
- Capture sleeps while no authenticated viewer is active and wakes on the next
  authorized request.
- Optional LinuxMice OIDC that issues a normal bounded local session.
- Example units: `packaging/systemd/rustopviewer-host.service` and
  `packaging/systemd/rustopviewer-agent.service`.

## Important UX Decision

Host env holding `DALE_TOKEN` does **not** log a phone browser in by itself.
Browsers must still prove access:

- paste the Masterdale bearer once per origin, or
- follow a fleet dashboard deep-link that includes `#masterdale=...` once, or
- use LinuxMice OIDC.

## Recent Changes

- `src/fleet.rs` registry + agent heartbeat
- CLI: `host`, `agent`, `status`, `devices`, `open`
- Dashboard fleet list in `assets/remote.html`
- Always bind Tailscale IPs even when an unrelated `tailscale serve` exists

## Verification Baseline

```sh
cargo test
cargo clippy --all-targets -- -D warnings
git diff --check
```
