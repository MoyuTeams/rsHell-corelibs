# rsHell core libraries

This repository contains the reusable, non-GTK libraries for rsHell:

- `rshell-core`: application models, protocols, and orchestration ports
- `rshell-platform`: operating-system integration
- `rshell-session`: terminal engines, PTY and SSH session management
- `rshell-storage`: SQLite persistence, migrations, imports, and credential storage

`third_party/portable-pty-psmux` is the pinned PTY fork used by `rshell-session`; its patch
provenance is documented inside that directory.

## Build and test

The workspace requires Rust 1.99.0 for the Windows selected-helper budget certificate.

```text
cargo fmt --all -- --check
cargo check --workspace --all-targets --all-features --locked
cargo test --workspace --all-features --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
pwsh -NoProfile -File scripts/qa/terminal-engine-gate.ps1
```

## Maintenance

Keep dependency revisions and exact versions pinned unless an intentional dependency update is
being made. Preserve the storage migrations in order, and update `Cargo.lock` from the existing
lockfile so unrelated packages are not upgraded. Production Rust modules are limited to 250
nonblank, non-comment lines by `production_module_limits`.
