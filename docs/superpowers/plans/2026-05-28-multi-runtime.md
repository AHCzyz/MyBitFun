---
change: multi-runtime-support
design-doc: docs/superpowers/specs/2026-05-28-multi-runtime-design.md
base-ref: 95c40184b8f731f2ad54ef28a4d07fe0975abf12
archived-with: 2026-05-29-multi-runtime-support
---

# Multi-Runtime Support — Implementation Plan

## Execution Order

Tasks are ordered by dependency. Each group must complete before the next begins.

### Group 1: Trait Foundation (runtime-ports crate)
All runtime adapters depend on these types compiling. No behavior, pure interfaces.

| Task | Files | Description |
|---|---|---|
| 1.1 | `src/crates/runtime-ports/src/agent_runtime.rs` (new) | `AgentRuntime` trait: `id`, `display_name`, `capabilities`, `create_session`, `health_check`, `shutdown` |
| 1.2 | same | `AgentSession` trait: `session_id`, `prompt` → `Stream<Item=AgentEvent>`, `steer`, `abort`, `dispose` |
| 1.3 | same | `AgentEvent` enum (8 variants + metadata), `StopReason` enum, `RuntimeCapabilities` struct |
| 1.4 | same | `SessionConfig` DTO (runtime_id, model_id, working_dir) |
| 1.5 | `src/crates/runtime-ports/src/lib.rs` | `pub mod agent_runtime` + re-exports |
| 1.6 | test | Unit test: trait compiles, AgentEvent serializes round-trip |

### Group 2: RuntimeRegistry (runtime-ports crate)
Registry depends on AgentRuntime trait from Group 1.

| Task | Files | Description |
|---|---|---|
| 2.1 | `src/crates/runtime-ports/src/registry.rs` (new) | `RuntimeRegistry` struct: `register`, `get`, `list_all` |
| 2.2 | same | `get_global_runtime_registry()` singleton (OnceLock) |
| 2.3 | same | `health_check_all()` + `select_default()` (OMP → Claude → BitFun) |
| 2.4 | `src/crates/runtime-ports/src/lib.rs` | Re-export registry types |

### Group 3: BitfunRuntime Adapter (core crate)
Wraps existing ExecutionEngine. Depends on Group 1 traits.

| Task | Files | Description |
|---|---|---|
| 3.1 | `src/crates/core/src/agentic/runtime_adapters/bitfun_runtime.rs` (new) | `BitfunRuntime` struct holding `Arc<AgenticSystem>` |
| 3.2 | same | `BitfunSession` wrapping `ExecutionEngine`, translating `AgenticEvent` → `AgentEvent` |
| 3.3 | same | `health_check()` always Ok, `capabilities()` reflects BitFun feature set |
| 3.4 | test | Compile test + session creation test |

### Group 4: OmpRuntime Adapter (core crate or new crate)
OMP subprocess bridge. Depends on Group 1 traits only.

| Task | Files | Description |
|---|---|---|
| 4.1 | `src/crates/core/src/agentic/runtime_adapters/omp_process.rs` (new) | `OmpProcess`: spawn `omp --mode rpc --no-session`, manage stdin/stdout JSONL |
| 4.2 | same | JSONL event reader loop: translate OMP events → `AgentEvent` |
| 4.3 | `src/crates/core/src/agentic/runtime_adapters/omp_runtime.rs` (new) | `OmpRuntime` implementing `AgentRuntime` |
| 4.4 | same | `OmpSession` implementing `AgentSession` |
| 4.5 | same | `health_check()`: detect `omp` in PATH |
| 4.6 | test | Integration test (requires `omp` binary installed) |

### Group 5: ClaudeRuntime Adapter (core crate or new crate)
Claude bridge subprocess. Depends on Group 1 traits only.

| Task | Files | Description |
|---|---|---|
| 5.1 | `resources/claude-bridge/bridge.mjs` (new) | Node.js bridge: `query()` → JSONL stdout |
| 5.2 | `src/crates/core/src/agentic/runtime_adapters/claude_process.rs` (new) | `ClaudeProcess`: spawn `node bridge.mjs`, manage lifecycle |
| 5.3 | same | Claude SDK event → `AgentEvent` translation |
| 5.4 | `src/crates/core/src/agentic/runtime_adapters/claude_runtime.rs` (new) | `ClaudeRuntime` + `ClaudeSession` |
| 5.5 | same | `health_check()`: detect `node` + `ANTHROPIC_API_KEY` |
| 5.6 | test | Integration test (requires API key) |

### Group 6: Coordinator Integration
Connects RuntimeRegistry to existing session creation path.

| Task | Files | Description |
|---|---|---|
| 6.1 | `src/crates/core/src/agentic/coordination/coordinator.rs` | Accept `runtime_id` in session creation, delegate to RuntimeRegistry |
| 6.2 | `src/crates/core/src/agentic/coordination/scheduler.rs` | Add `runtime_id: Option<String>` to `QueuedTurn` |
| 6.3 | `src/crates/api-layer/src/dto.rs` | Add `runtimeId` to session creation DTO |
| 6.4 | `src/apps/desktop/src/api/agentic_api.rs` | Pass runtime_id through Tauri command |
| 6.5 | `src/crates/core/src/agentic/system.rs` | Register adapters in RuntimeRegistry at init |

### Group 7: Frontend Runtime Selector

| Task | Files | Description |
|---|---|---|
| 7.1 | `src/web-ui/src/flow_chat/components/RuntimeSelector.tsx` (new) | React component: list runtimes, health indicators |
| 7.2 | `src/apps/desktop/src/api/runtime_api.rs` (new) | Tauri command: `list_runtimes` → health_check_all() |
| 7.3 | `src/web-ui/src/flow_chat/components/ChatHeader.tsx` | Show current runtime badge, click to switch |
| 7.4 | `src/web-ui/src/flow_chat/store/` | Wire runtime_id into session creation flow |
| 7.5 | same | Gray out unavailable runtimes with reason tooltip |

### Group 8: Startup & Packaging

| Task | Files | Description |
|---|---|---|
| 8.1 | `src/crates/core/src/agentic/system.rs` | Trigger health_check_all() during AgenticSystem::init |
| 8.2 | `src/apps/desktop/tauri.conf.json` | Bundle `resources/claude-bridge/` as app resource |

## Risk Areas

| Area | Risk | Mitigation |
|---|---|---|
| OMP process lifecycle | Process hangs on malformed input | Watchdog timeout + force kill + restart |
| Claude bridge npm deps | `@anthropic-ai/claude-agent-sdk` needs install | Bundle node_modules or use single-file bridge |
| Event translation gaps | OMP/Claude events not mapped to AgentEvent | metadata HashMap captures unmapped fields |
| Coordinator coupling | Large coordinator.rs changes risk regressions | Minimal change: only session creation path, not turn execution |
