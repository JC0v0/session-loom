# Repository Guidelines

## Project Structure & Module Organization

- `src/` contains the TypeScript core. Agent-specific readers and writers live under `src/adapters/{claude,codex}/`; canonical session types and serialization live in `src/canonical/`; daemon, CLI, and SQLite storage code use their matching subdirectories.
- `src-tauri/` contains the Rust/Tauri desktop shell and bundling configuration.
- `ui/` contains the desktop interface. Edit `ui/theme.css`, then run `node scripts/inline-theme.cjs` to update the inlined styles in `ui/index.html`.
- `bin/ssl.js` is the installed CLI shim. `assets/` and `src-tauri/icons/` hold application artwork. Design decisions are recorded in `docs/plans/`.
- Tests are colocated with TypeScript modules as `*.test.ts`.

## Build, Test, and Development Commands

- `npm install` installs JavaScript tooling.
- `npm start -- <args>` runs the CLI through `tsx`; for example, `npm start -- list`.
- `npm run desktop` starts the Tauri application in development mode.
- `npm test` runs all Vitest tests once.
- `npm run typecheck` checks strict TypeScript without emitting files.
- `npm run bundle:cli` creates `dist-cli/cli.mjs`, used by the packaged desktop app.
- `npm run dist` builds the desktop application for the current platform. On Windows it creates the NSIS installer.
- `npm run dist:mac` builds the macOS application bundle and DMG installer.
- `cargo fmt --manifest-path src-tauri/Cargo.toml -- --check` validates Rust formatting.

## Coding Style & Naming Conventions

Use two-space indentation, single quotes, semicolons, and explicit TypeScript types at module boundaries. Keep TypeScript strict and prefer small, focused modules. Use `camelCase` for functions and variables, `PascalCase` for types, and lowercase tool identifiers (`claude`, `codex`). Rust follows `rustfmt` and `snake_case`. No repository-wide JavaScript formatter or linter is configured, so preserve nearby style and rely on type checking and focused review.

## Testing Guidelines

Use Vitest and name files `<module>.test.ts`. Add adapter fixtures for format changes and regression tests for watcher, storage, restore, and serialization behavior. Tests must isolate filesystem state with environment overrides such as `SESSION_LOOM_STORE`, `CODEX_SESSIONS_ROOT`, and `CLAUDE_ROOT`. Run `npm test` and `npm run typecheck` before submitting changes.

## Commit & Pull Request Guidelines

Follow the existing Conventional Commit pattern: `feat(desktop): ...`, `fix(desktop): ...`, `test(session-bridge): ...`, or `chore: ...`. Keep commits scoped and imperative. Pull requests should explain the user-visible outcome, implementation or root cause, and verification performed. Link relevant issues or plans; include screenshots for UI changes and note any migration or session-format compatibility impact.

## Security & Local Data

Never commit session databases, agent histories, credentials, or generated user conversations. Runtime data belongs in `~/.session-loom/`; build outputs (`dist-cli/`, `src-tauri/target/`) remain untracked.
