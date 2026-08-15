---
title: Provider-Neutral Codex Restore
type: fix
date: 2026-08-15
topic: session-bridge
artifact_contract: ce-unified-plan/v1
artifact_readiness: implementation-ready
product_contract_source: user-directed
execution: code
---

# Provider-Neutral Codex Restore

## Goal Capsule

- **Objective:** Codex sessions saved by Session Loom must not pin a model provider or model, so a restored session is resumable in any project regardless of that project's `config.toml`.
- **Product authority:** The user. The user directed that every session Session Loom saves uses the Codex default provider and an empty model.
- **Open blockers:** None.

## Key Decision

- Restored Codex rollouts omit `model_provider` and `base_instructions` from the `session_meta` record. Codex resolves a rollout without these fields to the importing project's default provider and model (its `BaseInstructions` fallback documented in `codex-rs/protocol/src/protocol.rs`), so the session appears under any project's default provider in `codex resume --all` instead of being hidden by the picker's provider filter.
- The canonical store still records `model_provider` and `model` from the source session as metadata; the omission applies only to the write adapter (`codex::write_session`).

## Verification

- `cargo test --workspace`, `cargo fmt --all -- --check`, and `cargo clippy --workspace --all-targets -- -D warnings` pass.
- Adapter tests assert the written `session_meta` carries neither `model_provider` nor `base_instructions`, even when the canonical session records both.
