# Contributing to rustopviewer

Thanks for helping build RustOp Viewer (ROV).

## Before You Start

- Read the [README](README.md) for project goals and current limitations.
- Check open issues and pull requests before starting work.
- For larger changes, open an issue first so we can align on scope and design.

## Development Setup

ROV currently targets Windows 11.

1. Install a current Rust toolchain.
2. Clone the repository.
3. Run:

```powershell
cargo fmt
cargo clippy --all-targets --all-features -- -D warnings
cargo test
cargo run
```

## Project Standards

- Keep changes focused and reviewable.
- Prefer clarity over cleverness.
- Add or update tests when behavior changes.
- Document user-facing changes in the README, docs, or changelog as appropriate.
- Avoid breaking public behavior without a migration plan.

## Pull Requests

- Use descriptive titles.
- Explain the user problem, the chosen solution, and any tradeoffs.
- Include screenshots or screen recordings for UI changes when possible.
- Call out risks, limitations, and follow-up work explicitly.

## Rust Style

- `cargo fmt` is required.
- `cargo clippy -- -D warnings` should pass.
- Avoid unnecessary dependencies.
- Prefer small modules with clear responsibilities.

## Security and Privacy

Because ROV controls real machines:

- Treat remote-input changes as security-sensitive.
- Be explicit about authentication and network-exposure implications.
- Never weaken access controls casually for convenience.

## Licensing

By contributing, you agree that your contributions will be licensed under the project's dual MIT or Apache-2.0 license.
