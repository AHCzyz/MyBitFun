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
