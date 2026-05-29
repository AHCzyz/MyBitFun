---
change: runtime-turn-persistence
design-doc: docs/superpowers/specs/2026-05-30-runtime-turn-persistence-design.md
base-ref: 70ee5ca0d99bf5863c9dbdbd6acb8f3f78baf1e5
---

# Runtime Turn Persistence (F-3) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Persist runtime (claude/OMP) assistant text to the session store on all three terminal paths (Completed / Cancelled / Failed) so it survives session reload.

**Architecture:** Plan B. `run_runtime_event_loop` accumulates streamed `TextDelta` into `acc_text` and calls `session_manager.{complete,cancel,fail}_dialog_turn` directly on each terminal path. A shared `inject_partial_text_if_absent` helper (extracted from `complete_dialog_turn`'s existing fallback) injects accumulated text into `model_rounds` guarded by `has_assistant_text` (no-op when text already present, so the bitfun path passing `None` is unaffected).

**Tech Stack:** Rust, crate `bitfun-core`. Test: `cargo test -p bitfun-core --lib`. Build: `cargo check -p bitfun-core --tests`.

---

## File Structure

- **Modify** `src/crates/core/src/agentic/session/session_manager.rs`:
  - Add free fn `inject_partial_text_if_absent(turn: &mut DialogTurnData, text: &str, ts: u64)`
  - Refactor `complete_dialog_turn` fallback (currently ~3083-3124) to call it (zero behaviour change)
  - Add `partial_text: Option<String>` param to `cancel_dialog_turn` (~3220) and `fail_dialog_turn` (~3153); call helper before setting status
  - Fix misleading comment above `cancel_dialog_turn` (D-3)
  - Add tests: complete characterization, cancel/fail-with-partial-text reload, empty-text no-round, None no-op
- **Modify** `src/crates/core/src/agentic/coordination/coordinator.rs`:
  - `persist_cancelled_dialog_turn` (~1846) / `persist_failed_dialog_turn` (~1924): pass `None` to the new param
  - `run_runtime_event_loop` (~174): add `session_manager: Arc<SessionManager>` param; accumulate `acc_text`; call persist methods on terminal paths
  - Spawn call site (~2990, inside `tokio::spawn`): clone `session_manager` for the helper
  - Test module: add `test_session_manager()` + `TestWorkspace`; thread param through T1-T7; add F-3 integration test

**Note on line numbers:** This file is 6000+ lines and shifts as edits land. Each task gives a grep anchor (a unique string to locate the edit) instead of relying on absolute line numbers.

---

## Task 1: Extract `inject_partial_text_if_absent` helper (zero behaviour change)

**Files:**
- Modify: `src/crates/core/src/agentic/session/session_manager.rs` (`complete_dialog_turn`, grep anchor `let has_assistant_text = turn.model_rounds`)
- Test: same file, `#[cfg(test)] mod tests`

The `complete_dialog_turn` fallback that synthesizes a final round has no dedicated test. Lock its behaviour with a characterization test FIRST, then extract the helper and confirm the test still passes (proves zero behaviour change). This is the V-5 regression net — it does not exist yet.

- [ ] **Step 1: Write the characterization test for the existing fallback**

Add to the `tests` mod in `session_manager.rs`. Pattern follows `start_dialog_turn_with_existing_context_persists_turn_and_snapshot` (uses `TestWorkspace`, `test_manager`, `create_session`, `start_dialog_turn`, `load_dialog_turn`).

```rust
#[tokio::test]
async fn complete_dialog_turn_injects_final_response_when_no_assistant_text() {
    let workspace = TestWorkspace::new();
    let persistence_manager =
        Arc::new(PersistenceManager::new(workspace.path_manager()).expect("persistence"));
    let manager = test_manager(persistence_manager.clone());
    let session = manager
        .create_session(
            "complete-fallback".to_string(),
            "agentic".to_string(),
            SessionConfig {
                workspace_path: Some(workspace.path().to_string_lossy().to_string()),
                ..Default::default()
            },
        )
        .await
        .expect("session");
    let turn_id = manager
        .start_dialog_turn(&session.session_id, "agentic".to_string(),
            "hello".to_string(), None, None, None)
        .await
        .expect("turn");

    manager
        .complete_dialog_turn(&session.session_id, &turn_id, "assistant reply".to_string(),
            TurnStats { total_rounds: 1, total_tools: 0, total_tokens: 0, duration_ms: 0 })
        .await
        .expect("complete");

    let reloaded = persistence_manager
        .load_dialog_turn(workspace.path(), &session.session_id, 0)
        .await.expect("load").expect("turn exists");
    let text: String = reloaded.model_rounds.iter()
        .flat_map(|r| r.text_items.iter())
        .map(|i| i.content.clone()).collect();
    assert_eq!(text, "assistant reply", "fallback must inject final_response as a text item");
    assert_eq!(reloaded.status, TurnStatus::Completed);
}
```

- [ ] **Step 2: Run it — verify it PASSES against current code**

Run: `cargo test -p bitfun-core --lib complete_dialog_turn_injects_final_response_when_no_assistant_text`
Expected: PASS (characterizes existing behaviour before refactor).

- [ ] **Step 3: Add the helper as a free fn in `session_manager.rs`**

Place above `impl SessionManager` or near the other free fns. The body is the existing fallback logic, parameterized on `text`/`ts`:

```rust
/// Inject `text` as a single synthetic "completed" model round, but only when
/// the turn has no existing non-empty assistant text. No-op for empty text or
/// when assistant text already exists (idempotent; safe for the bitfun path).
fn inject_partial_text_if_absent(turn: &mut DialogTurnData, text: &str, ts: u64) {
    if text.trim().is_empty() {
        return;
    }
    let has_assistant_text = turn.model_rounds.iter().any(|round| {
        round.text_items.iter().any(|item| !item.content.trim().is_empty())
    });
    if has_assistant_text {
        return;
    }
    let round_index = turn.model_rounds.len();
    turn.model_rounds.push(ModelRoundData {
        id: format!("{}-final-round", turn.turn_id),
        turn_id: turn.turn_id.clone(),
        round_index,
        timestamp: ts,
        text_items: vec![TextItemData {
            id: format!("{}-final-text", turn.turn_id),
            content: text.to_string(),
            is_streaming: false,
            timestamp: ts,
            is_markdown: true,
            order_index: Some(0),
            is_subagent_item: None,
            parent_task_tool_id: None,
            subagent_session_id: None,
            status: Some("completed".to_string()),
        }],
        tool_items: Vec::new(),
        thinking_items: Vec::new(),
        start_time: ts,
        end_time: Some(ts),
        duration_ms: Some(0),
        provider_id: None,
        model_id: None,
        model_alias: None,
        first_chunk_ms: None,
        first_visible_output_ms: None,
        stream_duration_ms: None,
        attempt_count: None,
        failure_category: None,
        token_details: None,
        status: "completed".to_string(),
    });
}
```

- [ ] **Step 4: Replace the inline fallback in `complete_dialog_turn` with a helper call**

Find the block starting `let has_assistant_text = turn.model_rounds` through the closing `}` of the `if !has_assistant_text && !final_response.trim().is_empty() {` block (the `turn.model_rounds.push(ModelRoundData { ... });`). Replace the whole `let has_assistant_text ...` + `if ... { push }` with:

```rust
inject_partial_text_if_absent(&mut turn, &final_response, completion_timestamp);
```

Leave `turn.status = TurnStatus::Completed;` and everything after unchanged.

- [ ] **Step 5: Run characterization test — verify still PASSES (zero behaviour change)**

Run: `cargo test -p bitfun-core --lib complete_dialog_turn_injects_final_response_when_no_assistant_text`
Expected: PASS (refactor preserved behaviour).

- [ ] **Step 6: Commit**

```bash
git add src/crates/core/src/agentic/session/session_manager.rs
git commit -m "refactor: extract inject_partial_text_if_absent from complete_dialog_turn"
```

## Task 2: Add `partial_text` to `cancel_dialog_turn` and `fail_dialog_turn`

**Files:**
- Modify: `src/crates/core/src/agentic/session/session_manager.rs` (`fail_dialog_turn` grep anchor `pub async fn fail_dialog_turn`, `cancel_dialog_turn` grep anchor `pub async fn cancel_dialog_turn`)
- Modify: `src/crates/core/src/agentic/coordination/coordinator.rs` (callers at grep anchors `.cancel_dialog_turn(session_id, turn_id)` and `.fail_dialog_turn(session_id, turn_id, error_text.clone())`)
- Test: `session_manager.rs` tests mod

These are the only two callers of the session-manager-level `cancel_dialog_turn(sid,tid)` and `fail_dialog_turn(sid,tid,err)` (verified by grep — all other `.cancel_dialog_turn(` hits are single-arg methods on `round_executor`/`tool_pipeline`/coordinator-RPC, unrelated). Adding the param breaks exactly these two call sites.

- [ ] **Step 1: Write failing tests for the new behaviour**

Add to `session_manager.rs` tests mod. Two tests: cancel-with-partial-text persists+reloads; cancel-with-None injects nothing.

```rust
#[tokio::test]
async fn cancel_dialog_turn_persists_partial_text() {
    let workspace = TestWorkspace::new();
    let pm = Arc::new(PersistenceManager::new(workspace.path_manager()).expect("pm"));
    let manager = test_manager(pm.clone());
    let session = manager.create_session("cancel-partial".into(), "agentic".into(),
        SessionConfig { workspace_path: Some(workspace.path().to_string_lossy().to_string()), ..Default::default() })
        .await.expect("session");
    let turn_id = manager.start_dialog_turn(&session.session_id, "agentic".into(),
        "q".into(), None, None, None).await.expect("turn");

    manager.cancel_dialog_turn(&session.session_id, &turn_id, Some("partial answer".to_string()))
        .await.expect("cancel");

    let reloaded = pm.load_dialog_turn(workspace.path(), &session.session_id, 0)
        .await.expect("load").expect("exists");
    let text: String = reloaded.model_rounds.iter()
        .flat_map(|r| r.text_items.iter()).map(|i| i.content.clone()).collect();
    assert_eq!(text, "partial answer", "cancelled turn must persist partial text");
    assert_eq!(reloaded.status, TurnStatus::Cancelled);
}

#[tokio::test]
async fn cancel_dialog_turn_with_none_injects_no_round() {
    let workspace = TestWorkspace::new();
    let pm = Arc::new(PersistenceManager::new(workspace.path_manager()).expect("pm"));
    let manager = test_manager(pm.clone());
    let session = manager.create_session("cancel-none".into(), "agentic".into(),
        SessionConfig { workspace_path: Some(workspace.path().to_string_lossy().to_string()), ..Default::default() })
        .await.expect("session");
    let turn_id = manager.start_dialog_turn(&session.session_id, "agentic".into(),
        "q".into(), None, None, None).await.expect("turn");

    manager.cancel_dialog_turn(&session.session_id, &turn_id, None).await.expect("cancel");

    let reloaded = pm.load_dialog_turn(workspace.path(), &session.session_id, 0)
        .await.expect("load").expect("exists");
    assert!(reloaded.model_rounds.iter().all(|r| r.text_items.is_empty()),
        "None partial_text must not inject a round (bitfun no-op)");
    assert_eq!(reloaded.status, TurnStatus::Cancelled);
}

#[tokio::test]
async fn fail_dialog_turn_persists_partial_text() {
    let workspace = TestWorkspace::new();
    let pm = Arc::new(PersistenceManager::new(workspace.path_manager()).expect("pm"));
    let manager = test_manager(pm.clone());
    let session = manager.create_session("fail-partial".into(), "agentic".into(),
        SessionConfig { workspace_path: Some(workspace.path().to_string_lossy().to_string()), ..Default::default() })
        .await.expect("session");
    let turn_id = manager.start_dialog_turn(&session.session_id, "agentic".into(),
        "q".into(), None, None, None).await.expect("turn");

    manager.fail_dialog_turn(&session.session_id, &turn_id, "boom".to_string(),
        Some("partial before error".to_string())).await.expect("fail");

    let reloaded = pm.load_dialog_turn(workspace.path(), &session.session_id, 0)
        .await.expect("load").expect("exists");
    let text: String = reloaded.model_rounds.iter()
        .flat_map(|r| r.text_items.iter()).map(|i| i.content.clone()).collect();
    assert_eq!(text, "partial before error");
    assert_eq!(reloaded.status, TurnStatus::Error);
}
```

- [ ] **Step 2: Run — verify they FAIL to compile (signature mismatch)**

Run: `cargo test -p bitfun-core --lib cancel_dialog_turn_persists_partial_text`
Expected: compile error — `cancel_dialog_turn` takes 2 args, 3 supplied.

- [ ] **Step 3: Add `partial_text` param + helper call to `cancel_dialog_turn`**

At grep anchor `pub async fn cancel_dialog_turn(&self, session_id: &str, turn_id: &str)`, change signature to:

```rust
    pub async fn cancel_dialog_turn(
        &self,
        session_id: &str,
        turn_id: &str,
        partial_text: Option<String>,
    ) -> BitFunResult<()> {
```

After the `let mut turn = ... load_dialog_turn ...` block and before `turn.status = TurnStatus::Cancelled;`, insert:

```rust
        if let Some(text) = partial_text.as_deref() {
            let ts = SystemTime::now().duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default().as_millis() as u64;
            inject_partial_text_if_absent(&mut turn, text, ts);
        }
```

- [ ] **Step 4: Add `partial_text` param + helper call to `fail_dialog_turn`**

At grep anchor `pub async fn fail_dialog_turn(&self, session_id: &str, turn_id: &str, error: String)`, add `partial_text: Option<String>` as the last param. After its `load_dialog_turn` block and before `turn.status = TurnStatus::Error;`, insert the same `if let Some(text) = partial_text.as_deref() { ... inject_partial_text_if_absent(&mut turn, text, ts); }` block.

- [ ] **Step 5: Update the two coordinator callers to pass `None`**

In `coordinator.rs` `persist_cancelled_dialog_turn`, grep anchor `.cancel_dialog_turn(session_id, turn_id)` → `.cancel_dialog_turn(session_id, turn_id, None)`.
In `persist_failed_dialog_turn`, grep anchor `.fail_dialog_turn(session_id, turn_id, error_text.clone())` → `.fail_dialog_turn(session_id, turn_id, error_text.clone(), None)`.

- [ ] **Step 6: Run the three new tests + build**

Run: `cargo test -p bitfun-core --lib dialog_turn_persists_partial_text && cargo test -p bitfun-core --lib cancel_dialog_turn_with_none_injects_no_round`
Expected: PASS. Then `cargo check -p bitfun-core --tests` → clean.

- [ ] **Step 7: Commit**

```bash
git add src/crates/core/src/agentic/session/session_manager.rs src/crates/core/src/agentic/coordination/coordinator.rs
git commit -m "feat: cancel/fail_dialog_turn accept optional partial_text"
```

## Task 3: Thread `session_manager` into `run_runtime_event_loop` (no behaviour change)

**Files:**
- Modify: `src/crates/core/src/agentic/coordination/coordinator.rs` (`run_runtime_event_loop` signature grep anchor `async fn run_runtime_event_loop(`; spawn site grep anchor `run_runtime_event_loop(` inside `tokio::spawn`)
- Test: same file `mod tests` (grep anchor `async fn runtime_event_loop_cancels_promptly`)

This task is pure wiring: add the param, clone at spawn, give the test module a `SessionManager` builder, thread the new arg through all 7 existing call sites (T1-T7). No persist calls yet — the goal is "compiles + all existing runtime_event_loop tests still pass" so the signature change is isolated from behaviour change.

- [ ] **Step 1: Add the param to the signature**

At grep anchor `async fn run_runtime_event_loop(`, add as the last param (after `runtime_id_for_log: String,`):

```rust
    session_manager: Arc<SessionManager>,
```

(Body doesn't use it yet — prefix `_session_manager` is NOT needed because Task 4 will use it; but to compile cleanly in this task, name it `session_manager` and add `let _ = &session_manager;` at the top of the fn body, removed in Task 4. Alternatively name it `_session_manager` here and rename in Task 4. Use the `let _ = &session_manager;` approach to avoid a rename.)

Add at the very top of the fn body (first line after `{`):

```rust
    let _ = &session_manager; // used in Task 4 (persist calls)
```

- [ ] **Step 2: Clone `session_manager` at the spawn site**

At the spawn site (grep anchor `let _guard = TurnLifecycleGuard::new(` — note `session_manager` is *moved* into `TurnLifecycleGuard::new`). BEFORE the `TurnLifecycleGuard::new(session_manager, ...)` line, the surrounding code clones fields for the spawn. Find where `let session_manager = self.session_manager.clone();` is bound for the spawn (grep anchor `let session_manager = self.session_manager.clone();`). Add a second clone right after it:

```rust
            let session_manager_for_loop = session_manager.clone();
```

Then in the `run_runtime_event_loop(...)` call inside the spawn, add `session_manager_for_loop,` as the last argument (after `runtime_id_for_log,`).

- [ ] **Step 3: Add a `SessionManager` builder + `TestWorkspace` to the coordinator test module**

In `coordinator.rs` `mod tests`, near the other runtime_event_loop test imports (grep anchor `use super::{run_runtime_event_loop, AgenticEvent, EventQueue, RuntimeEvent};`), add a minimal workspace + manager builder. Mirror `session_manager.rs`'s `TestWorkspace` (it is private to that module, so define a local one here).

```rust
    use crate::agentic::persistence::PersistenceManager;
    use crate::agentic::session::{SessionContextStore, SessionManager, SessionManagerConfig};
    use crate::infrastructure::PathManager;
    use crate::service::config::PromptCachePolicy;

    struct TestWs { _dir: tempfile::TempDir, path_manager: Arc<PathManager> }
    impl TestWs {
        fn new() -> Self {
            let dir = tempfile::tempdir().expect("tempdir");
            let pm = Arc::new(PathManager::with_root(dir.path()).expect("path manager"));
            Self { _dir: dir, path_manager: pm }
        }
    }

    fn test_session_manager() -> Arc<SessionManager> {
        let ws = TestWs::new();
        let pm = Arc::new(PersistenceManager::new(ws.path_manager.clone()).expect("pm"));
        Arc::new(SessionManager::new(
            Arc::new(SessionContextStore::new()),
            pm,
            SessionManagerConfig {
                max_active_sessions: 100,
                session_idle_timeout: std::time::Duration::from_secs(3600),
                auto_save_interval: std::time::Duration::from_secs(300),
                enable_persistence: true,
                prompt_cache_policy: PromptCachePolicy::default(),
            },
        ))
    }
```

NOTE: verify `PathManager::with_root` exists (grep `fn with_root` / `fn new` in `infrastructure`). If the constructor differs, copy the exact pattern from `session_manager.rs`'s `TestWorkspace::path_manager()` setup. Also confirm `tempfile` is a dev-dependency (it is used by existing `TestWorkspace`). If imports resolve differently, match the working `session_manager.rs` tests mod imports exactly.

- [ ] **Step 4: Thread the new arg through all 7 existing call sites (T1-T7)**

Each `run_runtime_event_loop(...)` / `tokio::spawn(run_runtime_event_loop(...))` call in the tests currently ends with `"sid".into(), "tid".into(), "claude".into(),`. Append `test_session_manager(),` as the final argument to every one. Grep all call sites: `run_runtime_event_loop(` within the tests mod (T1 cancels_promptly, T2 completes_cleanly, T3 disposes_on_error_event, T4 skips_prompt_when_precancelled, T5 prompt_err cancelled, T6 prompt_err failed, T7 stream_error cancelled).

- [ ] **Step 5: Build + run all runtime_event_loop tests**

Run: `cargo test -p bitfun-core --lib runtime_event_loop` then `cargo test -p bitfun-core --lib runtime_cancel_guard`
Expected: all PASS (signature threaded, no behaviour change). A throwaway `test_session_manager()` whose store has no matching session means any future persist call returns `NotFound` and is logged, not panicked — fine for these tests which assert on events/disposal, not persistence.

- [ ] **Step 6: Commit**

```bash
git add src/crates/core/src/agentic/coordination/coordinator.rs
git commit -m "refactor: thread session_manager into run_runtime_event_loop"
```

## Task 4: Accumulate `acc_text` and persist on every terminal path

**Files:**
- Modify: `src/crates/core/src/agentic/coordination/coordinator.rs` (`run_runtime_event_loop` body)
- Test: same file `mod tests`

This is the behavioural core. The bare session-manager methods `complete_dialog_turn` / `cancel_dialog_turn` / `fail_dialog_turn` only persist (load turn → inject text → set status → save); they do NOT touch session state or emit events (verified). So calling them directly from the loop is pure persistence — TurnLifecycleGuard still owns session-state reset, and the loop still owns event emission. No double-emit, no conflict.

Pass `Some(acc_text.clone())` uniformly; `inject_partial_text_if_absent` skips empty text, so the D8 / prompt-err paths (empty acc_text) persist only the turn *status*, not an empty round.

Terminal paths and their persist call:

| Path (grep anchor) | Persist call |
|---|---|
| D8 pre-prompt cancel (`turn cancelled before prompt`) | `cancel_dialog_turn(sid,tid, Some(acc_text.clone()))` |
| prompt() Err cancel recheck (`turn cancelled during prompt()`) | `cancel_dialog_turn(...)` |
| prompt() Err non-cancel (`let err_msg = e.to_string();` first occurrence) | `fail_dialog_turn(sid,tid,err_msg.clone(), Some(acc_text.clone()))` |
| loop cancel arm (`turn cancelled by user`) | `cancel_dialog_turn(...)` |
| TurnEnd Completed (`StopReason::Completed =>`) | `complete_dialog_turn(sid,tid, acc_text.clone(), stats)` |
| TurnEnd Aborted (`StopReason::Aborted =>`) | `cancel_dialog_turn(...)` |
| TurnEnd `_` failed + Error arm (`turn cancelled during stream error` is the recheck; the Failed emit follows) | `fail_dialog_turn(...)` |

- [ ] **Step 1: Write the failing integration test (V-1 completed via the loop)**

Add to `coordinator.rs` tests mod. Builds a REAL manager+pm+workspace inline (test_session_manager() hides its workspace, so don't use it here), creates a session+turn, streams text then TurnEnd Completed, runs the loop, reloads.

```rust
#[tokio::test]
async fn runtime_event_loop_persists_completed_text_for_reload() {
    use crate::agentic::persistence::PersistenceManager;
    use crate::agentic::session::{SessionContextStore, SessionManager, SessionManagerConfig};
    use crate::infrastructure::PathManager;
    use crate::service::config::PromptCachePolicy;
    use crate::service::session::SessionConfig;

    let dir = tempfile::tempdir().expect("tempdir");
    let path_manager = Arc::new(PathManager::with_root(dir.path()).expect("pm"));
    let pm = Arc::new(PersistenceManager::new(path_manager).expect("persistence"));
    let manager = Arc::new(SessionManager::new(
        Arc::new(SessionContextStore::new()), pm.clone(),
        SessionManagerConfig {
            max_active_sessions: 100,
            session_idle_timeout: std::time::Duration::from_secs(3600),
            auto_save_interval: std::time::Duration::from_secs(300),
            enable_persistence: true,
            prompt_cache_policy: PromptCachePolicy::default(),
        },
    ));
    let session = manager.create_session("rt-complete".into(), "agentic".into(),
        SessionConfig { workspace_path: Some(dir.path().to_string_lossy().to_string()), ..Default::default() })
        .await.expect("session");
    let turn_id = manager.start_dialog_turn(&session.session_id, "agentic".into(),
        "hi".into(), None, None, None).await.expect("turn");

    let (tx, rx) = mpsc::channel::<RuntimeEvent>(8);
    tx.send(RuntimeEvent::TextDelta { delta: "hello ".into(), metadata: HashMap::new() }).await.unwrap();
    tx.send(RuntimeEvent::TextDelta { delta: "world".into(), metadata: HashMap::new() }).await.unwrap();
    tx.send(RuntimeEvent::TurnEnd { stop_reason: StopReason::Completed, metadata: HashMap::new() }).await.unwrap();
    drop(tx);

    let disposed = Arc::new(AtomicBool::new(false));
    let prompt_called = Arc::new(AtomicBool::new(false));
    let session_box = fake_session(Some(rx), disposed.clone(), prompt_called.clone());
    let cancel = CancellationToken::new();
    let cancels: Arc<DashMap<String, CancellationToken>> = Arc::new(DashMap::new());
    let queue = Arc::new(EventQueue::new(EventQueueConfig::default()));
    let slot: Arc<tokio::sync::Mutex<Option<Box<dyn AgentSession>>>> =
        Arc::new(tokio::sync::Mutex::new(None));

    run_runtime_event_loop(
        session_box, "hi".into(), cancel, cancels,
        queue.clone(), slot.clone(),
        session.session_id.clone(), turn_id.clone(), "claude".into(),
        manager.clone(),
    ).await;

    let reloaded = pm.load_dialog_turn(dir.path(), &session.session_id, 0)
        .await.expect("load").expect("turn exists");
    let text: String = reloaded.model_rounds.iter()
        .flat_map(|r| r.text_items.iter()).map(|i| i.content.clone()).collect();
    assert_eq!(text, "hello world", "completed runtime turn must persist accumulated text");
}
```

- [ ] **Step 2: Run — verify it FAILS**

Run: `cargo test -p bitfun-core --lib runtime_event_loop_persists_completed_text_for_reload`
Expected: FAIL — reloaded text is empty (no persist call yet).

- [ ] **Step 3: Add accumulator; remove the Task-3 placeholder line**

Remove `let _ = &session_manager;`. After `let _cancel_guard = RuntimeCancelGuard::armed(...)` add:

```rust
    let mut acc_text = String::new();
```

In the `RuntimeEvent::TextDelta { delta, .. } =>` arm, BEFORE the existing `event_queue.enqueue(... TextChunk ...)`, capture the delta into acc (the arm moves `delta` into the event, so accumulate first):

```rust
                    RuntimeEvent::TextDelta { delta, .. } => {
                        acc_text.push_str(&delta);
                        let _ = event_queue.enqueue(
                            AgenticEvent::TextChunk { /* unchanged */ },
                            Some(EventPriority::Normal),
                        ).await;
                    }
```

(Keep the existing TextChunk body; only the `acc_text.push_str(&delta);` line is new and must come before `delta` is moved.)

- [ ] **Step 4: Add persist calls to each terminal path**

Insert each call right AFTER the corresponding `event_queue.enqueue(... DialogTurn{Completed,Cancelled,Failed} ...)` and BEFORE the `dispose()`/`break`/`return` on that path. Use these exact calls (errors are logged, never panic — persistence is best-effort):

D8 pre-prompt cancel arm — after its Cancelled enqueue, before `rt_session.dispose()`:
```rust
        if let Err(e) = session_manager.cancel_dialog_turn(&session_id, &turn_id, Some(acc_text.clone())).await {
            log::warn!("Runtime persist (pre-prompt cancel) failed: turn_id={}, error={}", turn_id, e);
        }
```

prompt() Err cancel-recheck arm — same call, same placement (after Cancelled enqueue, before dispose).

prompt() Err non-cancel arm — after the `DialogTurnFailed` enqueue, before `rt_session.dispose()`:
```rust
        if let Err(e) = session_manager.fail_dialog_turn(&session_id, &turn_id, err_msg.clone(), Some(acc_text.clone())).await {
            log::warn!("Runtime persist (prompt err) failed: turn_id={}, error={}", turn_id, e);
        }
```

loop cancel arm (`turn cancelled by user`) — after Cancelled enqueue, before dispose: the `cancel_dialog_turn(... Some(acc_text.clone()))` call.

TurnEnd `StopReason::Completed` arm — after the Completed enqueue, replace the trailing `"completed"` expr region by inserting before it:
```rust
        if let Err(e) = session_manager.complete_dialog_turn(
            &session_id, &turn_id, acc_text.clone(),
            crate::agentic::session::TurnStats { total_rounds: 1, total_tools: 0, total_tokens: 0, duration_ms: 0 },
        ).await {
            log::warn!("Runtime persist (completed) failed: turn_id={}, error={}", turn_id, e);
        }
```
(Confirm `TurnStats` import path; grep `use` in coordinator.rs — it is referenced at `persist_completed_dialog_turn` as bare `TurnStats`, so use that same path, not the `crate::agentic::session::` prefix if the bare name is already imported.)

TurnEnd `StopReason::Aborted` arm — after Cancelled enqueue: the `cancel_dialog_turn(... Some(acc_text.clone()))` call.

TurnEnd `_` arm AND `RuntimeEvent::Error` arm — after their `DialogTurnFailed` enqueue, before dispose/break:
```rust
        if let Err(e) = session_manager.fail_dialog_turn(&session_id, &turn_id, err_msg.clone(), Some(acc_text.clone())).await {
            log::warn!("Runtime persist (failed) failed: turn_id={}, error={}", turn_id, e);
        }
```
(In the `RuntimeEvent::Error` arm the variable is `message`, not `err_msg` — use `message.clone()` there.)

- [ ] **Step 5: Run the integration test — verify PASS**

Run: `cargo test -p bitfun-core --lib runtime_event_loop_persists_completed_text_for_reload`
Expected: PASS.

- [ ] **Step 6: Add the cancelled-path integration test (V-2)**

Same scaffold as Step 1 but: stream one `TextDelta { delta: "partial" }`, keep `tx` alive (no TurnEnd), spawn the loop, `sleep(10ms)`, `cancel.cancel()`, await. Then reload and assert text == "partial" and `reloaded.status == TurnStatus::Cancelled`. (Mirror T1's spawn+cancel timing.)

```rust
#[tokio::test]
async fn runtime_event_loop_persists_partial_text_on_cancel() {
    // ... same manager/session/turn setup as Step 1 ...
    let (_tx, rx) = mpsc::channel::<RuntimeEvent>(8);
    _tx.send(RuntimeEvent::TextDelta { delta: "partial".into(), metadata: HashMap::new() }).await.unwrap();
    // keep _tx alive so the stream stays open and the cancel arm wins
    // ... build session_box, cancel, cancels, queue, slot ...
    let task = tokio::spawn(run_runtime_event_loop(
        session_box, "hi".into(), cancel.clone(), cancels,
        queue.clone(), slot.clone(),
        session.session_id.clone(), turn_id.clone(), "claude".into(), manager.clone(),
    ));
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    cancel.cancel();
    tokio::time::timeout(std::time::Duration::from_millis(200), task).await.expect("exit").expect("join");
    let reloaded = pm.load_dialog_turn(dir.path(), &session.session_id, 0).await.expect("load").expect("exists");
    let text: String = reloaded.model_rounds.iter().flat_map(|r| r.text_items.iter()).map(|i| i.content.clone()).collect();
    assert_eq!(text, "partial");
    assert_eq!(reloaded.status, crate::service::session::TurnStatus::Cancelled);
}
```

- [ ] **Step 7: Run both integration tests + full runtime_event_loop suite**

Run: `cargo test -p bitfun-core --lib runtime_event_loop`
Expected: all PASS (T1-T7 unchanged + 2 new persist tests).

- [ ] **Step 8: Commit**

```bash
git add src/crates/core/src/agentic/coordination/coordinator.rs
git commit -m "feat: persist runtime turn assistant text on all terminal paths (F-3)"
```

## Task 5: Correct the misleading `cancel_dialog_turn` comment (D-3)

**Files:**
- Modify: `src/crates/core/src/agentic/session/session_manager.rs` (doc comment above `pub async fn cancel_dialog_turn`)

The D-3 spike proved no production path writes `model_rounds` incrementally; the existing comment "Any partial assistant content that was already streamed is preserved in `model_rounds`" is unsupported. Replace it with the verified behaviour.

- [ ] **Step 1: Replace the doc comment**

Find the comment block above `pub async fn cancel_dialog_turn` (grep anchor `already streamed is preserved`). Replace the whole doc comment with:

```rust
    /// Mark a dialog turn as cancelled and persist it. Unlike
    /// `complete_dialog_turn`, this writes `TurnStatus::Cancelled` so the
    /// frontend / persistence layer can distinguish a user-cancelled turn
    /// from a fully-completed one. The turn's existing `model_rounds` are
    /// persisted as-is; runtime (claude/OMP) turns, whose model_rounds are
    /// otherwise empty, supply their streamed-so-far text via `partial_text`,
    /// which is injected only when no assistant text already exists.
```

- [ ] **Step 2: Build (no test needed — comment only)**

Run: `cargo check -p bitfun-core --tests`
Expected: clean.

- [ ] **Step 3: Commit**

```bash
git add src/crates/core/src/agentic/session/session_manager.rs
git commit -m "docs: correct cancel_dialog_turn comment re: partial-text persistence (D-3)"
```

---

## Task 6: Full build + test gate

**Files:** none (verification only)

- [ ] **Step 1: Full compile with tests**

Run: `cargo check -p bitfun-core --tests`
Expected: clean, no warnings introduced by this change.

- [ ] **Step 2: Run the full coordinator + session_manager test surface**

Run: `cargo test -p bitfun-core --lib coordinator::tests`
Expected: PASS (T1-T7 + 2 new persist integration tests).

Run: `cargo test -p bitfun-core --lib agentic::session`
Expected: PASS (complete characterization + 3 new cancel/fail partial-text tests + existing).

- [ ] **Step 3: Tick the OpenSpec tasks.md**

Edit `openspec/changes/runtime-turn-persistence/tasks.md` — mark D-1..D-5 and I-1..I-7, V-1..V-6 as complete (they are covered: D-1..D-4 in design, D-5 = this plan + delta spec; I-1..I-7 across Tasks 1-5; V-1=Task4 Step1, V-2=Task4 Step6, V-3 covered by session_manager fail test (Task2) + reload, V-4=Task2 None test, V-5=Task1 characterization, V-6=Task2 cancel-with-None).

- [ ] **Step 4: Commit**

```bash
git add openspec/changes/runtime-turn-persistence/tasks.md
git commit -m "chore: mark runtime-turn-persistence tasks complete"
```

---

## Self-Review

**Spec coverage** (delta spec requirements → task):
- "persisted on completion" → Task 4 Step 1/3/4 (Completed arm) + Task 4 Step 1 test
- "persisted on cancellation" → Task 4 (cancel arms) + Task 2 cancel test + Task 4 Step 6 test
- "persisted on failure" → Task 4 (Error/`_` arms) + Task 2 fail test
- "idempotent against existing assistant text" → Task 1 helper `has_assistant_text` guard + Task 2 None test
- "no empty round" scenario → Task 1 helper empty-text guard + Task 2 cancel-None test
- "bitfun unaffected" → Task 2 Step 5 (callers pass None) + Task 2 None test

**Placeholder scan:** No TBD/TODO. Every code step has concrete code. Two flagged verifications (not placeholders, real risks to confirm during impl): `PathManager::with_root` constructor name (Task 3 Step 3) and `TurnStats` import path (Task 4 Step 4) — both have fallback instructions ("match the working session_manager.rs pattern").

**Type consistency:** `inject_partial_text_if_absent(&mut DialogTurnData, &str, u64)` — defined Task 1, called Task 1/2 consistently. `cancel_dialog_turn(sid,tid,Option<String>)` / `fail_dialog_turn(sid,tid,String,Option<String>)` — signatures defined Task 2, called Task 4 consistently. `run_runtime_event_loop(..., Arc<SessionManager>)` — defined Task 3, called Task 3 (tests) + Task 4 (tests) consistently.

**Scope check:** Single capability, 2 source files. Focused. No decomposition needed.





