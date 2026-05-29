## Tasks

- [x] A-1: 新增 `runtime_cancel_guard_removes_when_armed` 和 `runtime_cancel_guard_keeps_when_disarmed` 两个 sync 测试
- [x] A-2: T1（runtime_event_loop_cancels_promptly）和 T4（runtime_event_loop_skips_prompt_when_precancelled）预置 cancels entry + helper 后断言移除
- [x] B: prompt() Err 臂开头加 `is_cancelled()` 复检，emit Cancelled 而非 Failed
