# 验证报告：runtime-turn-persistence (F-3)

> **日期：** 2026-05-30
> **workflow：** full
> **verify_mode：** full(delta spec 新 capability + 7 改动文件,真实达到完整验证阈值)
> **结论：** PASS — All checks passed, ready for archive

## 范围

让 runtime(claude/OMP)对话轮在 Completed/Cancelled/Failed 三条终止路径把累积的助手 **text + thinking** 写回 session store,会话重载后可见。提交区间 base-ref `70ee5ca0`...HEAD:7 文件、1402+/140-。源码 2 文件(coordinator.rs + session_manager.rs),其余为 change artifacts。

## 范围演进(mid-build scope revision)

初版设计 D-1=A(只 text)。build 中途用户实测发现 release 版**助理模式(=runtime 路径)持久化 thinking 且客户端重启仍在**,**专业模式(=bitfun)从不产生 thinking**。这推翻了"只做 text 以与 bitfun 一致"的前提(bitfun 根本无 thinking),故 scope 扩展为 **B-extended:text + thinking**。delta spec + Design Doc + plan 已同步更新(commit 86b8dcec)。

## 完整验证(openspec-verify-change 三维)

### Completeness:19/19 tasks ✓,4/4 requirements 实现

### Correctness:requirement → 实现 → 测试映射

| Delta-spec requirement | 实现 | 测试 |
|---|---|---|
| R1 Completed 持久化 text+thinking | coordinator.rs TurnEnd Completed 臂 `complete_dialog_turn(..., Some(acc_thinking), stats)` | runtime_event_loop_persists_completed_text_and_thinking_for_reload |
| R2 Cancellation 持久化 partial | 5 处 cancel_dialog_turn(D8/prompt-err/loop/Aborted/stop_reason/Error 臂) | runtime_event_loop_persists_partial_content_on_cancel + cancel_dialog_turn_persists_partial_text_and_thinking |
| R3 Failure 持久化 partial | 4 处 fail_dialog_turn(prompt-err/stop_reason/Error 臂) | fail_dialog_turn_persists_partial_text_and_thinking |
| R4 幂等(不覆盖已有 text) | inject_partial_content_if_absent 的 has_assistant_text 守卫 | cancel_dialog_turn_with_none_injects_no_round |

9 处终止路径 persist 调用 + 12 处 acc_thinking 引用,确认全路径接线。

### Coherence:Design Doc(B-extended)与实现一致;delta spec 与 design 无矛盾

- helper `inject_partial_content_if_absent(turn, text, thinking, ts)` 与设计一致
- complete/cancel/fail 加 thinking/partial_thinking 参数,bitfun 调用点传 None(no-op),与设计一致
- D-3 注释已按 spike 结论修正

## 轻量验证 5 项(交叉确认)

| # | 检查 | 结果 |
|---|---|---|
| 1 | tasks.md 全 `[x]` | ✓ 19/19 |
| 2 | 改动文件与 tasks 一致 | ✓ coordinator.rs + session_manager.rs |
| 3 | 编译 | ✓ cargo check -p bitfun-core --tests 干净 |
| 4 | 测试 | ✓ coordinator 18/0,session_manager 61/0 |
| 5 | 无安全问题 | ✓ 无 unsafe、无硬编码密钥 |

## 问题

- CRITICAL:无
- WARNING:无
- SUGGESTION:design 阶段 handoff hash 冻结于 B-extended 前的 delta spec——这是预期的(delta spec 是 build 期活文档),不影响 verify guard。

## 诚实标注(承自设计阶段)

- release 的 thinking 持久化机制无法从当前 repo 看到(release 从更早/不同版本构建)。本实现走 `model_rounds[].thinking_items`(reload 已读此路径)。若未来 release 代码合入冲突,以届时实际代码调和。
- **未做、留未来 feature:** 工具调用输出 + 子代理输出的 runtime 持久化(本期只 text+thinking);bitfun 自身 cancel/fail 部分内容持久化(传 None 保持现状)。

## 提交序列

| commit | 内容 |
|---|---|
| 86b8dcec | B-extended scope 修订(delta spec + design + plan) |
| 26d93636 | T1 抽 inject_partial_content_if_absent helper + characterization test |
| 4f978581 | T2 complete/cancel/fail 加 thinking/partial 参数 + reload 测试 |
| a1c63a4a | T3 接线 session_manager 进 run_runtime_event_loop |
| 7e90eb6b | T4 累积 acc_text+acc_thinking + 各终止路径持久化 + 集成测试 |
| 44c0ec1e | T5 修正 cancel_dialog_turn 注释(D-3) |
| 34d11135 | T6 勾选 tasks |

## 验证命令

```bash
cargo check -p bitfun-core --tests
cargo test -p bitfun-core --lib coordinator::tests   # 18/0
cargo test -p bitfun-core --lib agentic::session     # 61/0
```
