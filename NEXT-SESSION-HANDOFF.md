# NEXT SESSION HANDOFF — review3 follow-up + γ batch + F-3 persistence

> **Last updated:** 2026-05-29
> **Repo state:** `MyBitFun` on `main`, +11 ahead of `origin/main` after this push (or +12 if the workflow-docs commit is included). All three review3 batches archived under `openspec/changes/archive/`.

---

## 已完成（本会话 + 历史）

main 上的 11 个本地 commits（旧→新）：

| commit | 内容 | OpenSpec archive |
|---|---|---|
| `4883f414` | B 组 P2/P6/P7/P9 HTTPS 加固 | `2026-05-29-harden-runtime-resource-fetch` |
| `97f50a69` | B 组 P3 OMP SHA256 校验 | 同上 |
| `0b4ff520` | WIP baseline（代提交） | — |
| `82970140` | A 组 P1+P5+E0063 session 生命周期 | `2026-05-29-fix-runtime-session-lifecycle` |
| `cb2832ae` | A 组 P5-B kill_on_drop | 同上 |
| `2c154358` | α-1 review2 P5 错误分类 | `2026-05-29-fix-runtime-error-categorization` |
| `28e68756` | α-2 review2 P1 bridge timeout | `2026-05-29-fix-claude-bridge-timeout` |
| `0d88f2f5` | P4 runtime_sessions 清理 | `2026-05-29-fix-runtime-session-cleanup-on-delete` |
| `bbbfcf2d` | review3 batch1 P-1/P-4/P-5/P-7 | `2026-05-29-fix-review3-batch1` |
| `d4b95828` | review3 batch2 P-3 TurnLifecycleGuard panic-safety | `2026-05-29-fix-runtime-turn-registration` |
| `17a3667f` | review3 batch3 P-2 runtime turn cancellation | `2026-05-29-fix-runtime-turn-cancellation` |

加 1 个 docs commit (本次)：把 `openspec/`、`docs/superpowers/`、`review*.md` 移进 MyBitFun。`.claude/`（Comet skill 脚本 + 本地权限）保留在仓库外，另一台机器已配置好。

---

## 待做（按优先级）

### 优先级 1：review4 Finding A + B follow-up（小 hotfix，~40 行）

来自 `review4.md` 的"强烈推荐（本 commit 范围内）"，应作为 review3 P-2 的紧跟收尾：

**A. RuntimeCancelGuard 单测 + T1/T4 augment**
- 新增 2 个 sync `#[test]`（不带 tokio runtime）：
  - `runtime_cancel_guard_removes_when_armed` — pre-insert entry，drop guard，断言 entry removed
  - `runtime_cancel_guard_keeps_when_disarmed` — pre-insert，disarm，drop，断言 entry kept
- 在 T1 和 T4 中预置 `cancels.insert("tid", cancel.clone())`，helper 返回后 `assert!(cancels.get("tid").is_none())`
- **位置：** `coordinator.rs` mod tests（~5950 行附近）
- **目的：** 把 F-1 最微妙的 calling-thread guard / disarm / Step 3.5 的 RAII 移除行为纳入回归网

**B. prompt() Err 臂加 is_cancelled 复检**
```rust
Err(e) => {
    // B (review4): D8 is_cancelled() 通过后、prompt() 返回 Err 之前，
    // cancel 可能已触发。这里复检让事件归类为 Cancelled 而非 Failed。
    if cancel_token.is_cancelled() {
        log::info!("Runtime {} turn cancelled during prompt(): session_id={}, turn_id={}", ...);
        let _ = event_queue.enqueue(AgenticEvent::DialogTurnCancelled { ... }, Some(EventPriority::High)).await;
        let _ = rt_session.dispose().await;
        return;
    }
    let err_msg = e.to_string();
    let category = ...
    // existing emit DialogTurnFailed
}
```
- **位置：** `coordinator.rs` `run_runtime_event_loop` 的 prompt() Err 分支
- **3-5 行**

**估时：** 30 min。`/comet-hotfix` 路径。

---

### 优先级 2：γ 组 review2 P6/P7/P8（`/comet-tweak`）

来自原 session handoff，未做：

- **P6** bridge stdout reader 加 `max_line_length`（tokio `BufReader` limit；防恶意/损坏 stdin 让 readline 内存膨胀）
- **P7** OMP `agent_end` 区分 `Completed`/`Error`（当前总是 Completed，丢失错误信号）
- **P8** `RuntimeSelector` 健康状态 debounce（避免 UI 抖动）

跨模块（bridge.mjs / omp_runtime.rs / RuntimeSelector），不与 A+B 混。

**估时：** 30-45 min。

---

### 优先级 3：F-3 runtime turn 持久化（独立 `/comet`，**最高用户影响**）

来自 review3 设计文档 + review4 实证确认：

**问题：** `run_runtime_event_loop` 在 success/Error/Cancelled 任何路径都不调 `session_manager`。`complete_dialog_turn` 只在 bitfun 路径。runtime turn（claude/OMP）的助手回复仅以 `TextChunk` 事件流出，**会话重载后助手内容全部消失**。取消功能放大了这个 gap（取消的部分文本同样丢失）。

**实证（review4）：**
- `grep` 全仓：`DialogTurnCompleted`/`Cancelled` 的订阅者只有 `cron/subscriber.rs`（更新调度任务状态）和 `bitfun_runtime.rs`（事件→事件翻译）
- helper 体内 `session_manager.` 命中数 = 0

**修复方向：** 让 helper 在 turn 终止时（`TurnEnd { Completed }`、`TurnEnd { Aborted }`、`Error`、cancel branch、D8 pre-prompt）调 `session_manager.complete_dialog_turn` / `persist_cancelled_dialog_turn` / `persist_failed_dialog_turn`，把累积的 `TextChunk` 内容写回 session store。

**牵涉：**
- 累积 text deltas in helper（保留 ordering、thinking 内容）
- `session_manager.complete_dialog_turn` 当前签名（接受 ExecutionResult）—— runtime 路径需要构造对应 result，或新加 runtime 专用 API
- 测试：runtime turn → reload session → 助手回复存在
- 设计：是否需要 delta spec？（很可能需要——这是新 capability）

**估时：** Full `/comet` flow（设计 + 实施 + 验证 + 归档），1-2 小时。

---

### 优先级 4：其他长期项（review3 + review4）

- **review3 §6 / review4 §3：** 收敛 runtime 三张 DashMap（`runtime_sessions` + `active_turns_per_session` + `runtime_turn_cancels`）为 `RuntimeSessionEntry` 单一结构。降低 `delete_session` 同步面、关闭 review3 §P-6 race。
- **review3 §6 / review4 §3：** 让 runtime turn 接入 `ExecutionEngine` 的 cancel-token registry。
- **review4 Finding C：** EventQueue 对 Critical/High 做保留水位（避免极端负载下 `DialogTurnCancelled` 丢失）。
- **review4 Finding E：** `handle_user_input` runtime 分支的早返回 `?` 处补 `reset_session_state_if_processing` + emit `DialogTurnFailed`，避免 session 卡 Processing。
- **review3 §可疑模式 5：** bridge graceful abort window before kill（让 SDK 有 50ms graceful 时间）。

---

## 新设备启动建议

```bash
git clone <origin>/MyBitFun.git
cd MyBitFun

# 验证状态
git log --oneline -3       # 应看到 17a3667f 等 commits
ls openspec/changes/archive/   # 应看到 8 个归档 change
ls docs/superpowers/specs/     # 应看到设计文档
cat NEXT-SESSION-HANDOFF.md    # 这份

# .claude/skills（Comet/OpenSpec 工作流脚本）不在仓库里。
# 另一台机器已配好；新设备首次需要从该机器同步 .claude/skills/ 到
# MyBitFun/ 同级或父级，或确保 ~/.claude/skills/ 全局已装。

# 起步：A+B follow-up
# 直接 `/comet-hotfix` —— 单文件改动 ≤2 文件 ≤3 任务，hotfix 路径正合适
```

---

## 工作流文档结构

迁移后（本次 push 起）：

```
MyBitFun/                              ← git repo（main 跟踪）
├── src/                               ← 代码
├── docs/                              ← MyBitFun 原有 docs（已 tracked）
├── docs/superpowers/                  ← ★ 新增 tracked
│   ├── specs/                         ← 设计文档（每 change 一份）
│   ├── plans/                         ← 实施计划（每 change 一份）
│   └── reports/                       ← 验证报告（每 change 一份）
├── openspec/                          ← ★ 新增 tracked
│   ├── changes/archive/               ← 8 个归档 change
│   └── specs/runtime-resource-fetch/  ← 唯一一个 main spec
├── review1.md / review3.md / review4.md   ← ★ 新增 tracked
└── NEXT-SESSION-HANDOFF.md            ← ★ 本文档
```

**`.claude/`（Comet skill 脚本 + settings.local.json）保留在仓库外 + `.gitignore`**：
- `.claude/skills/`、`.claude/commands/` 是 Comet/OpenSpec 工作流脚本，另一台机器已配置；新设备需另行同步或全局安装。
- `.claude/settings.local.json` 含机器本地权限授予，不入库。

之前位于 `/f/Work/Mybitfun/` 顶层（与 MyBitFun/ 同级）的 `openspec/` 与 `docs/superpowers/` 迁移至 MyBitFun/ 内对应位置。`.comet.yaml` 中相对路径 `docs/superpowers/...` 仍然解析正确（基准从 `/f/Work/Mybitfun/` 改为 `MyBitFun/`，前提是从 MyBitFun/ 内调用 comet 脚本）。

---

## Review 历史

| review | 文件 | 范围 |
|---|---|---|
| review1 | `review1.md` | 多 runtime 初版功能 (commit `42432b0e`) — A/B/C 三组建议 |
| review3 | `review3.md` | A 组 hotfix（5 个 commits）— P-1 到 P-12 |
| review3 batch1 实施 | `bbbfcf2d` | review3 P-1, P-4, P-5, P-7 |
| review3 batch2 实施 | `d4b95828` | review3 P-3 |
| review3 batch3 实施 | `17a3667f` | review3 P-2 |
| review4 | `review4.md` | 对 batch3（commit `17a3667f`）的 post-merge 审核 — A 到 E |

review2.md 不存在于本仓库（它来自 α 组 (`2c154358`, `28e68756`) 的过程文档，未归档为 review*.md 文件）。
