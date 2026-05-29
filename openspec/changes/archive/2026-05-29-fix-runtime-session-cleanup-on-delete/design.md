## Context

Mapped during the explore phase. Per-session coordinator state and current cleanup status:

| Field | Type | Cleaned up on session delete? |
|---|---|---|
| `runtime_sessions` | `Arc<DashMap<String, Arc<Mutex<Option<Box<dyn AgentSession>>>>>>` | **No** (never `.remove()`d anywhere) |
| `active_turns_per_session` | `Arc<DashMap<String, Arc<AtomicUsize>>>` | **No** (never `.remove()`d anywhere) |
| `active_subagent_executions` | `Arc<DashMap<String, ActiveSubagentExecution>>` | Different keying (subagent execution, not session). RAII via `disarmed` / `abort_handle`. Out of scope. |

Call graph trace for "delete a live session":

```
Tauri command (agentic_api::delete_session)  ─┐
                                              │
delete_hidden_subagent_sessions_for_…  ───────┤
   (cascade: already calls cancel_active_…    │
    BEFORE delete_session)                    │
                                              ▼
                          coordinator.delete_session
                              │
                              ├── session_manager.delete_session
                              │     └── (8 internal cleanups —
                              │          context_store, prompt_cache,
                              │          file_read, persistence, cron,
                              │          terminal, sessions, workspace_index)
                              └── emit AgenticEvent::SessionDeleted
```

`session_api.rs::delete_session` (line 354) and `delete_archived_sessions` (line 587) bypass coordinator + session_manager entirely — they instantiate a `PersistenceManager` directly and only delete on-disk persistent data. They're "purge persisted data; this session is *not* loaded in memory" use cases. They cannot leave runtime_sessions entries because they never add to it (only the runtime-dispatch path inserts, which requires a loaded session). These do not bypass the proposed fix.

The cascade path at coordinator.rs:3454 already cancels active turns before deleting (`cancel_active_turn_for_session(sid, 2s)` then `delete_session`). The non-cascade entry point does not, so an in-flight turn races with deletion today even before the new cleanup. Aligning both paths is part of this fix.

## Goals / Non-Goals

**Goals:**
- Eliminate runtime_sessions and active_turns_per_session entries for a session at the moment of deletion.
- Match the cascade path's "cancel-before-delete" discipline at the regular delete path.
- Make the contract — *go through `coordinator.delete_session` to delete a live session* — explicit via doc comment.

**Non-Goals:**
- A SessionManager hook registry / observer pattern. Defers to future change if a second deletion path emerges.
- Migrate runtime_sessions ownership into SessionManager. Largest blast radius; not justified by current call topology.
- TTL or scavenger for stale entries. Process exit already cleans (DashMap drops; AgentSession drops; `kill_on_drop` reaps child). Live-process leak is the only failure mode this change targets.
- `active_subagent_executions` lifecycle audit. Different keying, different ownership pattern, separate concern.

## Decisions

### D1. Inline cleanup in `coordinator.delete_session`, not a hook

Rationale (from explore):
- Today only one call site reaches a live-session delete: `agentic_api::delete_session`. The hidden-subagent cascade re-enters via `self.delete_session(...)`, so it benefits from the same inline cleanup.
- A registration hook on SessionManager would protect against a future second caller, at a cost of ~30–50 lines plumbing and one new abstraction. With one caller today, that's premature.
- The doc comment (D3) makes the single-entry-point contract explicit so future PRs that add bypasses will be caught at review.

### D2. Order: cancel → coordinator-state cleanup → session_manager.delete_session → emit

```rust
pub async fn delete_session(...) -> BitFunResult<()> {
    // 1. Cancel in-flight turn so the runtime session quiesces.
    if let Err(e) = self
        .cancel_active_turn_for_session(session_id, Duration::from_secs(2))
        .await
    { warn!(...); /* fall through; cleanup is forced anyway */ }

    // 2. Remove + dispose the cached runtime session.
    if let Some((_, slot)) = self.runtime_sessions.remove(session_id) {
        if let Some(session) = slot.lock().await.take() {
            let _ = session.dispose().await;
        }
    }
    // 3. Remove the active-turn counter.
    self.active_turns_per_session.remove(session_id);

    // 4. Hand off to session_manager (existing behaviour).
    self.session_manager.delete_session(workspace_path, session_id).await?;
    self.emit_event(AgenticEvent::SessionDeleted { ... }).await;
    Ok(())
}
```

**Why cancel first:** disposing a session whose spawn task is mid-stream would have the spawn task writing events to a dying bridge. After A-group P1 fix, the spawn task's error path now properly decrements the counter and resets state, so racing isn't *fatal* — but cancelling first is cleaner and matches the cascade path's existing discipline.

**Why coordinator-state cleanup before session_manager.delete_session:** if `session_manager.delete_session` returns Err on its persistence step, the in-memory session_manager state may stay; in that case we accept that runtime_sessions has been eagerly cleaned. The runtime can always be re-created via the existing `or_insert_with` on the next `prompt()`, so there's no correctness hazard. Eager reclamation favours the leak-fix goal.

**Why explicit `dispose().await` rather than relying on `kill_on_drop`:** `dispose` cancels the abort_token (stops the background reader task cleanly) and explicitly kills the child. `kill_on_drop` only fires on drop and gives no chance for graceful shutdown. Use the better path when we have it; let `kill_on_drop` be the safety net for unexpected drop paths (panics, future code that bypasses dispose).

### D3. Doc comment marking the canonical entry point

Adds a single doc paragraph above `coordinator.delete_session`:

> Canonical entry point for deleting a *live* session — every coordinator-owned and session-manager-owned piece of per-session state is torn down here. Future deletion paths must go through this function. Direct calls to `session_manager.delete_session` (e.g. from `session_api`) are acceptable only when the session is known not to be loaded in memory, i.e. there is no `runtime_sessions` or `active_turns_per_session` entry for it.

## Risks / Trade-offs

- **Future bypass remains undetected by code.** Mitigated by D3's doc comment; not by a runtime check. Trade-off accepted.
- **`cancel_active_turn_for_session` failure swallowed.** Same pattern as the cascade path. The forced runtime cleanup below it still runs; the spawn task's error handling (post A-group P1 fix) still cleans up its own counter on the next stream event. Acceptable.
- **`dispose().await` may take up to a few hundred ms** (kills child, awaits kill). Adds to the user-perceived "delete session" latency. The user is already in a synchronous-feeling delete operation; the slowdown is bounded and small.
- **No unit/integration test added.** Same constraint as previous hotfixes — no runtime mocking infrastructure. Verified by code review + cargo check; behaviour proven by absence of search-grep matches for the leaked patterns post-fix.

## Migration / rollback

Single-commit revert. No data migration. No interface change.

## Open questions

None blocking. Tracked as follow-up:
- Audit `active_subagent_executions` lifecycle (different keying, may have its own leak shape).
- Re-evaluate hook-registry option if a second live-session deletion path is ever added.
- Verify that the desktop's "delete session" UI reaction time is acceptable with the added `dispose().await`; lower the cancel timeout from 2 s if needed.
