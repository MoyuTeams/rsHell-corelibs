# rsHell core libraries

This is a Rust 2024 workspace with four members:

- `crates/rshell-core`: domain types, protocols, application services, and ports
- `crates/rshell-platform`: platform paths, processes, permissions, clipboard, and shell behavior
- `crates/rshell-session`: local PTY and SSH actors plus terminal rendering
- `crates/rshell-storage`: SQLite repositories, migrations, imports, vaults, and helper processes

The vendored `third_party/portable-pty-psmux` crate is excluded from the workspace and consumed by
path from `rshell-session`. Keep its provenance file and dependency pins intact.

Use `cargo fmt --all -- --check`, then locked workspace check, test, and clippy commands. Run
`scripts/qa/terminal-engine-gate.ps1` after terminal-engine changes. Do not introduce GTK or desktop
release concerns here. Keep storage migrations append-only and retain the root `crypto-bigint`
patch. Production first-party Rust modules must remain within the 250-line policy enforced through
the `rshell-core` integration test target.
