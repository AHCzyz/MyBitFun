# Comet Design Handoff

- Change: runtime-turn-persistence
- Phase: design
- Mode: compact
- Context hash: 904dd26254359952fa1712b97046c23d0fb24b71318f15efd7dc1b3eff4c0965

Generated-by: comet-handoff.sh

OpenSpec remains the canonical capability spec. This handoff is a deterministic, source-traceable context pack, not an agent-authored summary.

## openspec/changes/runtime-turn-persistence/proposal.md

- Source: openspec/changes/runtime-turn-persistence/proposal.md
- Lines: 1-33
- SHA256: 36236be95f91462880e81e6e12cbdd26303cd0cf974d3f02799edc8f5e396645

```md
## Why

claude/OMP 运行时(非 bitfun)的对话轮在 `run_runtime_event_loop`(`coordinator.rs` 约 174-395 行)的任何终止路径——成功(`TurnEnd Completed`)、取消(`Cancelled`)、失败(`Error`/`StopReason::_`)——都**不调用 `session_manager`**。助手回复仅以 `TextChunk`/`ThinkingChunk` 事件流式发给 UI,从不写回 session store。后果:**会话重载后 claude/OMP 的助手内容全部消失**(reload 从 `model_rounds[].text_items[].content` 重建,而 runtime turn 的 `model_rounds` 恒空)。取消功能放大了这个 gap——取消时已生成的部分文本同样丢失。

实证(review4):grep 全仓,`DialogTurnCompleted`/`Cancelled` 订阅者只有 `cron/subscriber.rs` 和 `bitfun_runtime.rs`;`run_runtime_event_loop` 体内 `session_manager.` 命中数 = 0。

这是 review3/review4 路线图优先级 3、最高用户影响项(用户直接可感的数据丢失)。

## What Changes

- `run_runtime_event_loop` 累积流式的 text / thinking deltas(保留 ordering),在三条终止路径把累积内容写回 session store:
  - **Completed** → 复用现有 `session_manager.complete_dialog_turn(sid, tid, final_response, stats)`。其内建 `has_assistant_text` fallback(`model_rounds` 无文本 + `final_response` 非空 → 注入合成 round)正好命中 runtime turn(`model_rounds` 恒空),零 API 改动。
  - **Cancelled** → 扩展 `cancel_dialog_turn` 接受 `partial_text: Option<String>`,以 `has_assistant_text` 守卫注入(空才注入,对 bitfun 是 no-op)。
  - **Failed** → 扩展 `fail_dialog_turn` 接受 `partial_text: Option<String>`,同守卫。
- `run_runtime_event_loop` 签名新增 `session_manager: Arc<SessionManager>`(spawn body 已为 `TurnLifecycleGuard` clone 一份,再 clone 廉价)。
- 修正 `cancel_dialog_turn` 的误导性注释(详见 design 的矛盾注释处理 + spike)。

## Capabilities

### New Capabilities

- `runtime-turn-persistence`: 定义 runtime(claude/OMP)对话轮在成功/取消/失败终止时,助手 text+thinking 内容必须持久化到 session store 并在重载后可见的行为契约。

### Modified Capabilities

(无现有 spec 覆盖此行为;以新 capability 表达。)

## Impact

- `src/crates/core/src/agentic/coordination/coordinator.rs`:`run_runtime_event_loop`(累积 + 三路径持久化 + 加参数)、调用点(spawn body 加 `session_manager.clone()`)。
- `src/crates/core/src/agentic/session/session_manager.rs`:`cancel_dialog_turn`、`fail_dialog_turn` 加 `partial_text` 参数 + 注入逻辑 + 注释修正。
- 测试:runtime turn 三路径 → reload → 助手回复(含 thinking)存在;bitfun 路径 partial_text=None 不回归。
- 无 schema 变更(复用现有 `ModelRoundData`/`TextItemData` 结构)。
```

## openspec/changes/runtime-turn-persistence/design.md

- Source: openspec/changes/runtime-turn-persistence/design.md
- Lines: 1-51
- SHA256: f272bbfbded0d6af122368c6a2a57db49efc8c78c18576ca9fe3666b0e4459bd

```md
## 高层架构决策

### 方案选型:B(扩展 cancel/fail 接受 partial_text)

探索阶段比较了 3 个方案:

| | Completed | Cancelled/Failed | API 改动 | F-3 闭合度 |
|---|---|---|---|---|
| A 最小复用 | `complete_dialog_turn` | 调现有 cancel/fail,部分文本仍丢 | 仅 helper + 加参数 | 部分 |
| **B 扩展 cancel/fail(选定)** | `complete_dialog_turn` | cancel/fail 加 `partial_text: Option<String>`,`has_assistant_text` 守卫注入 | 改 2 API 签名 + 调用点 | **完全** |
| C 新建 runtime 专用 API | `persist_runtime_turn` | 同函数内分派 | 加 1 新 API | 完全但冗余 |

**选 B 的理由:** review 明确点名"取消功能放大了这个 gap",取消的部分文本丢失是用户痛点核心。A 留尾巴(取消 gap 未闭合);C 与 `complete_dialog_turn` 的 fallback 逻辑冗余。B 用 `has_assistant_text` 守卫(空才注入)对 bitfun(model_rounds 有内容)是 no-op、对 runtime(空)才注入,两路径兼容不重复写。

### 数据流

```
run_runtime_event_loop (claude/OMP spawn task)
  │  累积: acc_text:String, acc_thinking:String (按事件到达顺序)
  ├─ TextDelta    → emit TextChunk(不变) + acc_text.push_str(delta)
  ├─ ThinkingDelta→ emit ThinkingChunk(不变) + acc_thinking.push_str(delta)
  │
  ├─ TurnEnd Completed → complete_dialog_turn(sid,tid,acc_text,stats)
  │                       └ has_assistant_text fallback 注入 acc_text ✓
  ├─ cancel / D8 / prompt-err cancel → cancel_dialog_turn(sid,tid, Some(acc_text+thinking))
  ├─ Error / StopReason::_ → fail_dialog_turn(sid,tid,err, Some(acc_text+thinking))
  │
  └ reload 读: model_rounds[].text_items[].content + thinking_items[].content
```

### 集成点

- `run_runtime_event_loop` 签名加 `session_manager: Arc<SessionManager>`(spawn body 已 clone 给 `TurnLifecycleGuard`,再 clone 廉价)。
- `cancel_dialog_turn` / `fail_dialog_turn` 加 `partial_text: Option<String>` 参数;现有 bitfun 调用点传 `None`(no-op,不回归)。

### 待 brainstorming 深化的开放项

1. **thinking 内容的持久化形态** — `complete_dialog_turn` 的 fallback 只注入 `text_items`,不含 `thinking_items`。runtime 的 thinking 要不要持久化?若要,Completed 路径的 fallback 也需扩展(或 runtime 走不同注入)。这是方案 B 边界,需 design 定。
2. **ordering 保留** — text 与 thinking 交错到达时,reload 渲染顺序。当前 reload 把 text_items 和 thinking_items 分别 join,可能丢失交错顺序。
3. **矛盾注释 spike** — `cancel_dialog_turn` 注释"已流式内容保留在 model_rounds"无证据支撑(全仓无增量写 model_rounds 的生产路径)。design 阶段定向确认 bitfun 持久化机制(~15min spike),据此敲定注释措辞。判定不阻塞 B(`has_assistant_text` 守卫对两种真相都安全)。
4. **partial_text 注入逻辑落点** — 注入逻辑放 `cancel_dialog_turn`/`fail_dialog_turn` 内,还是抽共享 helper(与 complete 的 fallback 共用)。

### 不改动

- 不改 bitfun 执行引擎路径
- 不改 `ModelRoundData`/`TextItemData` 结构(复用)
- 不改事件流(TextChunk/ThinkingChunk emit 保持,只是额外累积)

### 升级/规模评估

跨 2 文件(coordinator.rs + session_manager.rs)、新 capability、需 delta spec、5+ 测试。完整 workflow(full),非 preset。
```

## openspec/changes/runtime-turn-persistence/tasks.md

- Source: openspec/changes/runtime-turn-persistence/tasks.md
- Lines: 1-26
- SHA256: f5ca6a06d32dad6aebde606f144199147e064bfa3315933ec2ca992d49b600b8

```md
## Tasks

> open 阶段的高层清单;design 阶段会据 Design Doc 细化、build 阶段再拆执行步骤。

### 设计阶段开放项(进 comet-design 解决)
- [ ] D-1: 定 thinking 内容持久化形态(Completed fallback 是否扩展 thinking_items / runtime 是否走不同注入)
- [ ] D-2: 定 text+thinking 交错 ordering 的保留与 reload 渲染策略
- [ ] D-3: spike 确认 bitfun 流式持久化机制(~15min),据此敲定 cancel_dialog_turn 注释最终措辞
- [ ] D-4: 定 partial_text 注入逻辑落点(内联 vs 抽共享 helper 与 complete fallback 共用)
- [ ] D-5: 产出 delta spec(runtime-turn-persistence capability)+ Design Doc

### 实现阶段(build 阶段细化)
- [ ] I-1: `run_runtime_event_loop` 签名加 `session_manager: Arc<SessionManager>`,调用点 spawn body clone 传入
- [ ] I-2: helper 内累积 acc_text / acc_thinking(保留 ordering)
- [ ] I-3: Completed 路径调 `complete_dialog_turn(sid, tid, acc_text, stats)`
- [ ] I-4: `cancel_dialog_turn` / `fail_dialog_turn` 加 `partial_text: Option<String>` + has_assistant_text 守卫注入;现有 bitfun 调用点传 None
- [ ] I-5: Cancelled 路径(cancel 臂 / D8 / prompt-err cancel)调 cancel_dialog_turn 带 partial_text
- [ ] I-6: Failed 路径(Error 臂 / StopReason::_)调 fail_dialog_turn 带 partial_text
- [ ] I-7: 修正 cancel_dialog_turn 注释(按 D-3 spike 结论)

### 验证阶段
- [ ] V-1: 测试 runtime Completed → reload → 助手回复存在
- [ ] V-2: 测试 runtime Cancelled(有部分文本)→ reload → 部分文本存在
- [ ] V-3: 测试 runtime Failed → reload → 已生成文本存在
- [ ] V-4: 测试 bitfun 路径 partial_text=None 不回归(cancel/fail 既有行为不变)
- [ ] V-5: 编译 + 运行 coordinator + session_manager mod tests 全绿
```

## openspec/changes/runtime-turn-persistence/specs/runtime-turn-persistence/spec.md

- Source: openspec/changes/runtime-turn-persistence/specs/runtime-turn-persistence/spec.md
- Lines: 1-49
- SHA256: 7584dcab588dc191ae73a0e1a525d5abdc01ef8a12d4933c679721af915d80de

```md
## ADDED Requirements

### Requirement: Runtime turn assistant text is persisted on completion

When a runtime (claude/OMP) dialog turn ends with `TurnEnd { StopReason::Completed }`, the assistant text streamed during the turn MUST be persisted to the session store so it is visible after the session is reloaded. The runtime event loop SHALL accumulate the `TextDelta` content it streams and pass it to `complete_dialog_turn` so the existing `has_assistant_text` fallback persists it as a model round.

#### Scenario: Completed runtime turn survives reload
- **WHEN** a runtime turn streams assistant text and ends with `StopReason::Completed`
- **THEN** the accumulated assistant text SHALL be written to the dialog turn's `model_rounds` and SHALL be present when the session is reloaded

#### Scenario: Completed runtime turn with no text produces no empty round
- **WHEN** a runtime turn ends with `StopReason::Completed` but streamed no assistant text
- **THEN** no synthetic empty model round SHALL be injected

### Requirement: Runtime turn partial text is persisted on cancellation

When a runtime dialog turn is cancelled after streaming partial assistant text, the partial text MUST be persisted so it is visible after reload. `cancel_dialog_turn` SHALL accept an optional `partial_text` and inject it into `model_rounds` only when the turn has no existing assistant text (so the bitfun path, which passes no text, is unaffected).

#### Scenario: Cancelled runtime turn preserves partial text
- **WHEN** a runtime turn streams partial assistant text and is then cancelled
- **THEN** the partial text SHALL be persisted to `model_rounds` with turn status `Cancelled`, and SHALL be present after reload

#### Scenario: Cancellation before any text streamed
- **WHEN** a runtime turn is cancelled before any assistant text is streamed (e.g. D8 pre-prompt or during prompt() error)
- **THEN** the turn status SHALL be set to `Cancelled` and no empty model round SHALL be injected

#### Scenario: bitfun cancellation is unaffected
- **WHEN** a bitfun turn is cancelled and `cancel_dialog_turn` is called with `partial_text = None`
- **THEN** the existing bitfun cancellation behaviour SHALL be unchanged (no new round injected)

### Requirement: Runtime turn partial text is persisted on failure

When a runtime dialog turn fails after streaming partial assistant text, the partial text MUST be persisted so it is visible after reload. `fail_dialog_turn` SHALL accept an optional `partial_text` and inject it into `model_rounds` only when the turn has no existing assistant text.

#### Scenario: Failed runtime turn preserves generated text
- **WHEN** a runtime turn streams assistant text and then ends with `RuntimeEvent::Error` or a non-Completed/Aborted `StopReason`
- **THEN** the generated text SHALL be persisted to `model_rounds` with turn status `Error`, and SHALL be present after reload

#### Scenario: bitfun failure is unaffected
- **WHEN** a bitfun turn fails and `fail_dialog_turn` is called with `partial_text = None`
- **THEN** the existing bitfun failure behaviour SHALL be unchanged (no new round injected)

### Requirement: Partial-text injection is idempotent against existing assistant text

The injection of `partial_text` MUST be guarded so it never overwrites or duplicates assistant text that already exists in `model_rounds`. Injection SHALL occur only when the turn currently has no non-empty assistant text item.

#### Scenario: Turn already has assistant text
- **WHEN** a dialog turn already contains a non-empty assistant `text_item` and a persist path is called with `partial_text = Some(...)`
- **THEN** no additional round SHALL be injected and the existing content SHALL be preserved unchanged
```

