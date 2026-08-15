# Repository Guidelines

## Project Structure & Module Organization

- `crates/session-loom-core/` contains the shared Rust domain logic: canonical sessions, Codex/Claude adapters, SQLite storage, restore, watcher, and daemon lifecycle.
- `crates/session-loom-cli/` contains the Rust `ssl` terminal application. The CLI and desktop shell both depend on `session-loom-core`.
- `src-tauri/` contains the Rust/Tauri desktop shell and bundling configuration. `src-tauri/binaries/` is populated with the platform-specific Rust CLI before development or release builds.
- `ui/` contains the desktop interface. Edit `ui/theme.css`, then run `node scripts/inline-theme.cjs` to update the inlined styles in `ui/index.html`.
- `ui/icon.png` and `src-tauri/icons/` hold application artwork. Design decisions are recorded in `docs/plans/`.
- Rust integration tests live under each crate's `tests/` directory; private unit tests stay beside their modules.

## Build, Test, and Development Commands

- `npm install` installs the Tauri build tool.
- `npm start -- <args>` runs the Rust CLI; for example, `npm start -- list`.
- `cargo run -p session-loom-cli -- <args>` runs `ssl` directly during development.
- `npm run desktop` starts the Tauri application in development mode.
- `npm test` or `cargo test --workspace` runs all Rust tests.
- `npm run dist` builds the desktop application for the current platform. On Windows it creates the NSIS installer.
- `npm run dist:mac` builds the macOS application bundle and DMG installer.
- `cargo fmt --all -- --check` validates Rust formatting, and `cargo clippy --workspace --all-targets -- -D warnings` runs static checks.

## Coding Style & Naming Conventions

Rust follows `rustfmt`, uses `snake_case` functions and modules, and keeps shared behavior in `session-loom-core` instead of duplicating it in the CLI or desktop shell. Serialized fields use camelCase for compatibility with the existing database and frontend. The UI and build scripts use two-space indentation, single quotes, and semicolons.

## Testing Guidelines

Add Rust integration coverage for adapter format changes and regressions in watcher, storage, restore, daemon, and serialization behavior. Tests must isolate filesystem state with temporary directories and environment overrides such as `SESSION_LOOM_STORE`, `CODEX_SESSIONS_ROOT`, and `CLAUDE_ROOT`. Run workspace tests, rustfmt, and Clippy before submitting changes.

## Commit & Pull Request Guidelines

Follow the existing Conventional Commit pattern: `feat(desktop): ...`, `fix(desktop): ...`, `test(session-bridge): ...`, or `chore: ...`. Keep commits scoped and imperative. Pull requests should explain the user-visible outcome, implementation or root cause, and verification performed. Link relevant issues or plans; include screenshots for UI changes and note any migration or session-format compatibility impact.

## Security & Local Data

Never commit session databases, agent histories, credentials, generated user conversations, or generated CLI binaries. Runtime data belongs in `~/.session-loom/`; build outputs (`target/`, `src-tauri/binaries/ssl*`) remain untracked.
