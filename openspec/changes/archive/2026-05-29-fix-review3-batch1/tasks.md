## 1. Hotfix

- [x] 1.1 P-1: Rewrite `classify_runtime_error` in coordinator.rs — kind-first, message-only for Backend/None.
- [x] 1.2 P-5: Change `RuntimeEvent::Error` branch from `break` to `dispose + fetch_sub + reset_state + return` (matching prompt() Err semantics).
- [x] 1.3 P-7: Fix `delete_session` doc comment — session_api.rs goes through PersistenceManager directly, not session_manager.delete_session.
- [x] 1.4 P-4: In bridge.mjs, wrap `iter.return?.()` in `Promise.race([..., new Promise(r => setTimeout(r, 2000))])`.
- [x] 1.5 Verify: `cargo check -p bitfun-core` + `node --check resources/claude-bridge/bridge.mjs` both exit 0.
