# Changelog

All notable changes to this project will be documented in this file.

The format is inspired by Keep a Changelog and this project aims to follow Semantic Versioning over time.

## [Unreleased]

### Added

- Cross-platform host TUI for RustOp Viewer.
- Browser-based remote web UI served by the host runtime.
- Screen capture pipeline for the selected monitor.
- Mouse, keyboard, scroll, and text injection support.
- Tailscale-friendly connection workflow.
- Initial open-source repository scaffolding and governance docs.
- Linux host support at the code level alongside the existing Windows host support.
- Desktop-browser wheel, right-click, and middle-click controls in the built-in client.
- Persistent trusted-browser records that can reissue normal sessions after a one-time pairing.
- A supported `--headless` runtime for unattended deployments that already trust at least one browser.
- A `--print-pair-code` startup option for issuing one host-approved pairing code while running headless.
- Support for `ROV_EXTRA_LISTEN_ADDRS` so trusted local reverse proxies can reach the host runtime without widening to a normal LAN bind.

### Changed

- Replaced the native desktop host window with a terminal-first host control surface.
- Reworked input injection to use the cross-platform `enigo` backend instead of a Windows-only `SendInput` path.
- Generalized host and browser copy from a phone-only Windows workflow to Linux/Windows hosts with desktop or mobile browsers.
- Updated the browser client to use relative API paths so it can sit behind a stripped reverse-proxy prefix.
- Added host-side trust revocation and browser-side remembered-device session restoration.
