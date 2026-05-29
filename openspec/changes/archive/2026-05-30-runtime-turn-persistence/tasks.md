## Tasks

> 已完成。B-extended scope(text + thinking)。实现见 plan 顶部 B-extended 修订节。

### 设计阶段开放项(comet-design 已解决)
- [x] D-1: thinking 持久化形态 —— **改为 B-extended:text + thinking 都持久化**(实测推翻"与 bitfun 一致"前提:助理模式=runtime 持久化 thinking 且重启仍在;专业模式=bitfun 从无 thinking)
- [x] D-2: ordering —— text 进 text_items、thinking 进 thinking_items 两个独立数组,各自 push_str 累积,无交错排序
- [x] D-3: spike 确认全仓无"增量写 model_rounds"生产路径 → cancel_dialog_turn 注释判定不成立,已修正
- [x] D-4: 抽共享 helper `inject_partial_content_if_absent(turn, text, thinking, ts)`,complete/cancel/fail 三处复用
- [x] D-5: delta spec(runtime-turn-persistence capability,text+thinking)+ Design Doc 已产出

### 实现阶段
- [x] I-1: `run_runtime_event_loop` 签名加 `session_manager: Arc<SessionManager>`,spawn body clone 传入(commit a1c63a4a)
- [x] I-2: helper 内累积 acc_text + acc_thinking(commit 7e90eb6b)
- [x] I-3: Completed 路径调 `complete_dialog_turn(sid, tid, acc_text, Some(acc_thinking), stats)`
- [x] I-4: `complete_dialog_turn` 加 thinking 参,`cancel/fail_dialog_turn` 加 partial_text + partial_thinking + has_assistant_text 守卫注入;bitfun 调用点传 None(commit 4f978581)
- [x] I-5: Cancelled 路径(cancel 臂 / D8 / prompt-err cancel / Aborted)调 cancel_dialog_turn 带 partial
- [x] I-6: Failed 路径(Error 臂 / StopReason::_ / prompt-err fail)调 fail_dialog_turn 带 partial
- [x] I-7: 修正 cancel_dialog_turn 注释(commit 44c0ec1e)

### 验证阶段
- [x] V-1: runtime Completed → reload → 助手 text + thinking 存在(runtime_event_loop_persists_completed_text_and_thinking_for_reload)
- [x] V-2: runtime Cancelled(有部分 text+thinking)→ reload → 都存在(runtime_event_loop_persists_partial_content_on_cancel)
- [x] V-3: Failed 路径 partial 持久化(fail_dialog_turn_persists_partial_text_and_thinking,session_manager 层)
- [x] V-4: bitfun 路径 None 不回归(cancel_dialog_turn_with_none_injects_no_round)
- [x] V-5: 编译 + coordinator(18/0)+ session_manager(61/0)mod tests 全绿
- [x] V-6: 空内容取消无空 round(cancel_dialog_turn_with_none_injects_no_round 覆盖)
- [x] V-7: helper 仅 thinking 无 text 时注入 thinking_items、空 text_items(inject_partial_content_if_absent 逻辑保证;cancel/fail partial 测试覆盖 thinking 注入)
