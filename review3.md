# 极致代码审查报告 — α 组 hotfix（review3）

审查范围：`origin/main..HEAD` 9 个 commit，重点四枚 hotfix：
- `82970140` — runtime session 生命周期（review1 P1+P5）
- `cb2832ae` — `kill_on_drop` 安全网
- `2c154358` — DialogTurnFailed 错误分类（review2 P5）
- `28e68756` — bridge.mjs first-event/idle timeout（review2 P1）
- `0d88f2f5` — `delete_session` 清理 runtime_sessions / active_turns（review1 P4）

---

## 1. 总体评估

- **代码质量评分：68 / 100**（每个 patch 单独看 80+，但合在一起留下了若干路径覆盖盲点和一处死代码）
- **最关键风险：**
  1. **🔴 `classify_runtime_error` 的 `match kind { … }` 整段是死代码**：`classify_ai_error_message` 从不返回 `Unknown`（兜底就是 `ModelError`），早返回守卫永真，结构化 PortErrorKind 映射永不命中。最常见的两个运行时错误（缺 `ANTHROPIC_API_KEY`、缺 Node.js）会被错误分类成 `ModelError`，前端会引导用户「重试 / 切模型」，而真正的修复是「打开模型设置 / 装 Node」。
  2. **🟠 `delete_session` 不取消正在运行的 runtime spawn task**：`cancel_active_turn_for_session` 走的是 `execution_engine` / `tool_pipeline` 的 cancel API，runtime 路径根本没把 cancel token 接进去；`wait_session_drained` 顶多看到计数器、不能加速结束。结果：删除时若有活跃 turn，bridge 仍会消耗 API token 直到自然结束或撞上新加的 120 s timeout。`kill_on_drop` 兜底，但只有在 spawn task 自己跑完之后才生效。
  3. **🟠 runtime spawn task 仍非 panic 安全**：bitfun 路径用 `ActiveTurnRegistration` RAII 保护 `active_counter` 和 `reset_session_state_if_processing`；runtime 路径只在 `Ok` 和 `prompt() Err` 两条「正常」路径上手工对称。任何异步 `await` 抛 panic（包括 `event_queue.enqueue`、`stream.next` 内部）都会让 counter 永久 +1、session 永久卡 `Processing`。
- **审查摘要：** 四枚 hotfix 的方向都正确，单独 review 都过得去。但放在一起看：
  - 错误分类的早返回逻辑没看清 `classify_ai_error_message` 的契约 → 结构化 fallback 失效；
  - delete_session 的「canonical entry point」语义只覆盖了「拆资源」，没覆盖「打断在跑的 turn」；
  - bridge.mjs 的 `iter.return?.()` 没限时，把 `iter.next()` 上的 hang 风险换成了 `iter.return()` 上的 hang 风险；
  - runtime spawn task 在错误事件流（`RuntimeEvent::Error`）后仍把 session 放回缓存，不 dispose，下次 prompt() 大概率再失败一轮才能触发清理。

---

## 2. 发现的问题（按严重程度排序）

### P-1 🔴 严重 ｜ 逻辑-死代码 ｜ `classify_runtime_error` 结构化映射永不命中

- **位置：** `src/crates/core/src/agentic/coordination/coordinator.rs:69~83`（commit `2c154358`）
- **详细描述：**
  ```rust
  fn classify_runtime_error(message: &str, kind: Option<&PortErrorKind>) -> ErrorCategory {
      let from_message = classify_ai_error_message(message);
      if !matches!(from_message, ErrorCategory::Unknown) {  // ← 永真
          return from_message;
      }
      match kind { … }  // ← 永远到不了
  }
  ```
  看 `core-types/src/errors.rs:61~198`：`classify_ai_error_message` 整个函数体是一长串 `if/else if`，最末尾 `else { ErrorCategory::ModelError }`。**没有任何分支返回 `Unknown`**。所以 `from_message` 永远不是 `Unknown`，守卫永真，`match kind` 段是不可达代码。
- **关联上下文：** Display 实现是 `write!(f, "{:?}: {}", self.kind, self.message)`（`runtime-ports/src/lib.rs:42~44`）。三种最常见的 runtime 启动失败：
  - `PortErrorKind::PermissionDenied` + 消息 `"ANTHROPIC_API_KEY environment variable not set. …"` → 拼后小写 `"permissiondenied: anthropic_api_key environment variable not set."`，`classify` 各模式（`"permission denied"` 带空格、`"401"`、`"unauthorized"` 等）全不命中，落到 `ModelError`。**预期：`Auth`**。
  - `PortErrorKind::NotAvailable` + 消息 `"Node.js not found: not bundled and not in PATH"` → 没有任何 `service unavailable / overloaded / 503` 等 token，落到 `ModelError`。**预期：`ProviderUnavailable`**。
  - `PortErrorKind::NotFound` + 消息 `"Claude bridge script not found at …"` → 仍然不命中任何模式，落到 `ModelError`。**预期：`InvalidRequest`**（或单独一个 `MissingResource`）。
  侥幸命中的只有：`PortErrorKind::Timeout`（debug 字串 `"Timeout: "` 含 `"timeout"`，触发第 187 行）。
- **重现/证明：** 单测可直接证伪：
  ```rust
  #[test]
  fn classify_unset_api_key() {
      let e = PortError::new(
          PortErrorKind::PermissionDenied,
          "ANTHROPIC_API_KEY environment variable not set.",
      );
      assert_eq!(
          classify_runtime_error(&e.to_string(), Some(&e.kind)),
          ErrorCategory::Auth, // 实际会是 ModelError
      );
  }
  ```
- **后果：**
  - 前端结构化错误 UI 拿到 `ModelError`，按 `action_hints_for_category(ModelError)`（`errors.rs:244~246`）渲染 `["retry", "switch_model", "copy_diagnostics"]`。用户重试，再次失败；切模型也无效（因为根本没设环境变量）。**正确提示应是 `["open_model_settings", "copy_diagnostics"]`（Auth）或 `["wait_and_retry", "switch_model", "copy_diagnostics"]`（ProviderUnavailable）。**
  - telemetry 维度上整个 runtime 路径的错误几乎全归到 `ModelError`，category-keyed 仪表板全失真。
- **修复建议：**
  方案 A（最小改动，只改 classifier 的早返回判定）：
  ```rust
  fn classify_runtime_error(message: &str, kind: Option<&PortErrorKind>) -> ErrorCategory {
      // 结构化优先：PortErrorKind 是底层端口契约里更可靠的信号；
      // 消息字串只在 Backend / 无 kind 时才是唯一线索。
      match kind {
          Some(PortErrorKind::Timeout) => return ErrorCategory::Timeout,
          Some(PortErrorKind::PermissionDenied) => return ErrorCategory::Auth,
          Some(PortErrorKind::NotAvailable) => return ErrorCategory::ProviderUnavailable,
          Some(PortErrorKind::InvalidRequest) | Some(PortErrorKind::NotFound) => {
              return ErrorCategory::InvalidRequest
          }
          Some(PortErrorKind::Cancelled) => return ErrorCategory::Unknown,
          Some(PortErrorKind::Backend) | None => {} // 落到消息分析
      }
      let from_message = classify_ai_error_message(message);
      // classify 兜底是 ModelError；这里需要的就是这个兜底。
      from_message
  }
  ```
  方案 B（保留作者本意：消息优先、结构化兜底）：把 `classify_ai_error_message` 的兜底改成 `Unknown`，单独写一个 `_or_model_error` 的 wrapper 给 bitfun 路径调（避免影响现有调用方）。改动面更大，不推荐。
- **参考：** CWE-561 Dead Code。

---

### P-2 🟠 高危 ｜ 资源-异步泄漏 ｜ `delete_session` 不打断在跑的 runtime spawn task

- **位置：** `coordinator.rs:3424~3467`（`delete_session`），结合 `cancel_active_turn_for_session:3374~3408` 与 `cancel_dialog_turn:3276~3372`。
- **详细描述：**
  `delete_session` 第 1 步调用 `cancel_active_turn_for_session(session_id, 2s)`。这个函数：
  1. 把 session 状态改 `Idle`（`update_session_state_for_turn_if_processing`）；
  2. `execution_engine.cancel_dialog_turn(turn_id)` — runtime 路径**没有**把 turn 注册到 `execution_engine`（搜 `register_cancel_token`，runtime 分支不调用）→ no-op；
  3. `tool_pipeline.cancel_dialog_turn_tools(turn_id)` — runtime 路径不走 tool pipeline → no-op；
  4. `cancel_active_subagents_for_parent_turn` — runtime 路径不创建 subagent → no-op；
  5. `wait_session_drained(1500ms)` — 看到 counter > 0，循环等到 deadline，超时返回；
  6. `cancel_active_turn_for_session` 自己再追加 `has_active_turn` 轮询 2s — runtime 也没注册，立即返回。
  
  **结论：runtime spawn task 完全没收到任何取消信号。** 它会继续执行，bridge 进程仍在调 Claude API，直到 `stream.next()` 自然返回 `None` / `TurnEnd` / `Error`，或撞上新加的 120 s `IDLE_TIMEOUT_MS`。
- **关联上下文：**
  - `delete_session` 第 2 步 `runtime_sessions.remove(session_id)` → `slot.lock().await.take()`：此刻 spawn task 已经在 `coordinator.rs:2637` 把 session 从 slot 里 take 走了（slot 当前是 `None`），所以 take() 返回 `None`，**`dispose()` 不会被调用**。注释说「dispose() 干净地关掉 bridge」，但在「删除时正好有活跃 turn」这条最常见的路径上，dispose 根本没运行。
  - bridge child 最终是怎么死的？spawn task 跑完会走到 line 2810 `slot_guard.replace(rt_session)`，把 session 塞回到 slot。但那个 `Arc<Mutex<Option<…>>>` 已经从 DashMap 里 remove 了；只有 spawn task 自己持有 `session_slot_clone`。task 一返回，Arc 引用计数归零，`Box<dyn AgentSession>` drop → `kill_on_drop(true)` 触发 SIGKILL。
  - 所以**最坏窗口** = 「`delete_session` 返回」到「spawn task 自然结束」≈ up to 120 s（`IDLE_TIMEOUT_MS`）。这段时间：
    - Anthropic API 仍在被调用，**继续消耗用户 token / 配额**；
    - 用户在 UI 上看到的是「会话已删除」，但 stderr 仍有日志，遥测仍计入；
    - 下次启动同 session_id（不会发生，因为 UUID）则无影响。
- **重现/证明：**
  1. 起一个 `runtime_id="claude"` 会话，发一条复杂 prompt（思考时间 30 s+）；
  2. 在 turn 进行中（已收到几个 `text_delta`、未收到 `turn_end`）调 `delete_session`；
  3. `delete_session` 应在 ~3.5 s 内返回（2 s cancel + 1.5 s drain）；
  4. 实际观察：bridge 进程仍在 task 列表里，Claude API 调用计费仍在累加，直到 turn 自然完成或 idle timeout。
- **修复建议：**
  在 `runtime_sessions` 表里同时存一个 `CancellationToken`，spawn task 内部 `tokio::select! { _ = cancel.cancelled() => {…dispose…break }, ev = stream.next() => {…} }`；`delete_session` 在 step 1 之后、step 2 之前先 `cancel.cancel()`，然后再等 drain。最小改造方案（不改 trait）：
  ```rust
  // coordinator state:
  runtime_session_cancels: Arc<DashMap<String, CancellationToken>>,
  
  // handle_user_input 的 runtime 分支：
  let runtime_cancel = self.runtime_session_cancels
      .entry(session_id.clone())
      .or_insert_with(CancellationToken::new)
      .clone();
  let runtime_cancel_for_task = runtime_cancel.clone();
  tokio::spawn(async move {
      // … existing prompt() …
      loop {
          tokio::select! {
              _ = runtime_cancel_for_task.cancelled() => {
                  let _ = rt_session.dispose().await;  // 不 put-back
                  // counter / state 复位
                  break;
              }
              ev = stream.next() => match ev { … }
          }
      }
  });
  
  // delete_session:
  if let Some((_, tok)) = self.runtime_session_cancels.remove(session_id) {
      tok.cancel();
  }
  // 然后 wait_session_drained 才有意义
  ```
- **参考：** CWE-772 Missing Release of Resource、OWASP A05:2021 Security Misconfiguration（资源未释放）。

---

### P-3 🟠 高危 ｜ 逻辑-异常安全 ｜ runtime spawn task 缺少 panic-safe RAII

- **位置：** `coordinator.rs:2662~2821`（runtime 分支 spawn task）vs `coordinator.rs:2848~2867`（bitfun 路径的 `ActiveTurnRegistration`）。
- **详细描述：**
  bitfun 路径在 spawn task 入口构造 `ActiveTurnRegistration { armed: true }`，`Drop` 时若 `armed` 则 `counter.fetch_sub(1)`。runtime 路径只在三条路径上手工对称：
  - prompt() Err（`2683~2687`）：fetch_sub + reset ✓
  - 成功结束（`2816~2820`）：fetch_sub + reset ✓
  - 错误事件 `RuntimeEvent::Error` 后 `break` → 走成功路径的尾巴 ✓
  
  但凡有 panic（含 `await` 取消），任何中间 `await` 抛错都会跳过尾巴。具体可能的 panic 点：
  - `event_queue.enqueue(...).await` — 如果 event 总线满或 internal panic；
  - `stream.next().await` 内部 channel 关闭异常（一般不 panic 但 ReceiverStream 实现可能 fold panic）；
  - `rt_session.dispose().await` — `child.kill()` IO 错误已被 `_=` 吞掉，不会 panic；但 mutex.lock 可能 panic（poisoned lock）。
  - 未来如果有人在 loop 里加 `unwrap()` / `expect()` / 直接 panic，全没保护。
- **关联上下文：** `wait_session_drained` 完全依赖 counter；counter 卡在 +1 → 后续所有 `cancel_dialog_turn` 都会撞 1.5 s 超时；`reset_session_state_if_processing` 没跑 → session 在 SessionManager 视角永远 `Processing`，前端看到「转圈圈不停」。
  
  比 bitfun 路径更糟的是：bitfun 路径的 `ActiveTurnRegistration` 还会在 panic 时正确递减 counter，至少 wait_session_drained 是干净的；runtime 路径出 panic 后，要么用户关 app（kill_on_drop 救你），要么手动调 `delete_session`（也走不通，见 P-2）。
- **重现/证明：** 改一行 `2696` 的 `let _ = event_queue.enqueue(…)` 后面加 `panic!("test")`，跑一个 runtime turn，观察 wait_session_drained 永久超时、SessionManager 状态卡死。
- **修复建议：** 抽出一个共享 RAII 给两条路径用：
  ```rust
  struct TurnRegistration {
      counter: Arc<AtomicUsize>,
      session_manager: Arc<SessionManager>,
      session_id: String,
      turn_id: String,
      armed: bool,
  }
  impl TurnRegistration {
      fn new(counter: Arc<AtomicUsize>, sm: Arc<SessionManager>, sid: String, tid: String) -> Self {
          counter.fetch_add(1, Ordering::SeqCst);
          Self { counter, session_manager: sm, session_id: sid, turn_id: tid, armed: true }
      }
      fn disarm(&mut self) { self.armed = false; }
  }
  impl Drop for TurnRegistration {
      fn drop(&mut self) {
          if self.armed {
              self.counter.fetch_sub(1, Ordering::SeqCst);
              self.session_manager.reset_session_state_if_processing(
                  &self.session_id, &self.turn_id,
              );
          }
      }
  }
  ```
  在 spawn task 入口构造一次，移除原来的 fetch_add；正常结束/失败的尾巴只做 dispose / put-back，不再手工 fetch_sub / reset。
- **参考：** CWE-460 Improper Cleanup on Thrown Exception、Rust nomicon "Exception Safety"。

---

### P-4 🟠 高危 ｜ 逻辑-超时迁移 ｜ `iter.return?.()` 无超时，把 next 上的 hang 换成 return 上的 hang

- **位置：** `resources/claude-bridge/bridge.mjs:237~247`
- **详细描述：**
  ```js
  let step;
  try {
      step = await Promise.race([iter.next(), timeoutPromise]);
  } catch (err) {
      clearTimeout(timer);
      try { await iter.return?.(); } catch { /* ignore */ }  // ← 没限时
      throw err;
  }
  ```
  `Promise.race` 对 loser promise 没有取消语义。当 `timeoutPromise` 赢了：
  1. `iter.next()` 仍在运行（HTTP 请求、SSE 流），它的 Promise 会继续 settle；
  2. `iter.return?.()` 是异步生成器协议里的 cleanup 方法。Claude Agent SDK 的实现没法保证：很多 async iterator 的 `return()` 会先 await 完目前 in-flight 的 `next()`、再设置 `done:true` 返回；
  3. 如果 SDK 内部 `next()` 卡在 `await fetch(…)`，`iter.return()` 会跟着卡。
  
  结果：**timeout 触发后 bridge 进程仍然 hang，错误事件无法写到 stdout**。Rust 端 `stream.next()` 看到的还是「啥也没收到」，跟没加 timeout 一样。
  
  甚至更糟：`process.stdout.write` 在 catch 块外的 outer try/catch 里发生（`bridge.mjs:261~272`），iter.return 不返回，`throw err` 不抛出去，outer catch 不进，**用户永远看不到错误事件**，仅靠 Rust 端 reader 的 EOF（child 死了）才会终止——但 child 没死，因为 Node 进程仍 alive 等 `iter.return()`。
- **关联上下文：**
  - 配合 P-2，删除时根本没主动 kill bridge → 恶性循环。
  - 即使没删除，下一个 prompt 命令到 stdin 时，`for await (const line of rl)` 的下一次迭代会进来，但前一个 prompt 的 try/catch 还没退出 → 死锁串成跨 turn。
- **重现/证明：** 在测试环境 mock SDK 让 `iter.next()` 永远 pending、`iter.return()` 等待 next 完成（这是最常见实现）。设置 `BITFUN_CLAUDE_BRIDGE_FIRST_EVENT_TIMEOUT_MS=2000`，跑 prompt，观察 bridge 进程超时 2 s 后仍 alive、stdout 无 error/turn_end。
- **修复建议：**
  ```js
  } catch (err) {
      clearTimeout(timer);
      try {
          // 给 cleanup 一个独立的硬上限。SDK 实现可能 await 完 in-flight next 才返回；
          // 我们不能因此再 hang 一次。
          await Promise.race([
              iter.return?.() ?? Promise.resolve(),
              new Promise(r => setTimeout(r, 2000)),
          ]);
      } catch { /* ignore */ }
      throw err;
  }
  ```
  另外建议在 timeout 时**主动 process.exit(1)**（而不是仅 throw 给外层），让 Rust 端通过 EOF 立即看到 bridge 死亡 → `kill_on_drop` 接管 → 整个会话生命周期复位。这个方案更暴力但更可靠：
  ```js
  } catch (err) {
      // 写错误事件后强制退出，让 Rust 端通过 EOF 复位
      process.stdout.write(JSON.stringify({type:'error', message: err.message}) + '\n');
      process.stdout.write(JSON.stringify({type:'turn_end', stopReason:'error'}) + '\n');
      process.exit(1);
  }
  ```
  但这会让 bridge 一次只能跑一轮，session 复用失效，需要权衡。**推荐先加 cleanup 限时，监控线上 hang 是否仍然出现。**
- **参考：** CWE-833 Deadlock。

---

### P-5 🟡 中危 ｜ 逻辑-状态污染 ｜ `RuntimeEvent::Error` 后仍把 session put-back 到缓存

- **位置：** `coordinator.rs:2785~2802` + `2804~2814`
- **详细描述：** Error 事件 `break` 出循环后，落到统一的 put-back 段：
  ```rust
  let mut slot_guard = session_slot_clone.lock().await;
  let displaced = slot_guard.replace(rt_session);   // ← 不管成功失败都塞回去
  ```
  对比 prompt() Err 分支（`2679~2680`）的语义：「Session may be in a bad state, dispose instead of caching.」 但 `RuntimeEvent::Error` 同样意味着 SDK iterator 出错（例如限流、网络断），bridge 内部状态可能已经损坏：
  - 下一轮 `prompt()` 写入 stdin，bridge 仍在前一轮的 catch 块里没回到 readline 循环 → stdin 写入被 buffered 但永远不被读 → Rust 端看到的是 prompt 没失败、stream 但永远空 → 撞 IDLE_TIMEOUT_MS（120 s）才得救；
  - 即便 bridge 恢复了，SDK 客户端的 token 限流计数、重试退避状态在缓存的 ClaudeSession 里仍是脏的。
- **关联上下文：** 与 P-4 串联——P-4 让 bridge 容易 hang；P-5 让脏 bridge 被复用；下一轮再撞 P-4。两者一起把「单 turn 失败」放大成「连续 N turn 全部 120 s 超时」。
- **重现/证明：** mock bridge 一次性输出 `{type:"error","message":"rate limit"}` + `{type:"turn_end","stopReason":"error"}`，下一轮 prompt 写入 stdin。在 bridge 的 readline 循环外的 try/catch 已经捕获并重新进入循环（实际 bridge 处理 OK），但更严苛的失败模式（SDK 内部 panic、worker 退出）会卡。
- **修复建议：** 把 `RuntimeEvent::Error` 也走 dispose 路径，与 prompt() Err 对称：
  ```rust
  RuntimeEvent::Error { message, .. } => {
      let category = classify_runtime_error(&message, None);
      let detail = ai_error_detail_from_message(&message, category.clone());
      let _ = event_queue.enqueue( /* DialogTurnFailed */, ).await;
      // 与 prompt() Err 对称：session 进入污染状态，dispose 而不是 put-back
      let _ = rt_session.dispose().await;
      active_counter.fetch_sub(1, Ordering::SeqCst);
      session_manager.reset_session_state_if_processing(
          &session_id_clone, &turn_id_clone,
      );
      return;
  }
  ```
  注意要 early `return` 跳过下方的 put-back / fetch_sub / reset，不能只 break，否则会重复递减计数器。
- **参考：** OWASP A04:2021 Insecure Design（错误恢复策略不对称）。

---

### P-6 🟡 中危 ｜ 并发-race ｜ `delete_session` 与并发 `handle_user_input` 重新插入

- **位置：** `coordinator.rs:3448~3452` 与 `2631~2634` 的 `or_insert_with`
- **详细描述：** 两条路径同时跑：
  - T1 `delete_session.runtime_sessions.remove(sid)` 拿到 `(_, slot_a)`；
  - T2 `handle_user_input.runtime_sessions.entry(sid).or_insert_with(...)` 这时 entry 不存在 → 插入新 `slot_b`；
  - T1 继续 `slot_a.lock().take().dispose()`，但 slot_a 已经从 map 中拿走、不会再有人用，dispose 的是上一个 turn 缓存的旧 session（可能没问题）；
  - T2 在 slot_b 上跑新 turn，spawn 一个新 bridge child 进程；
  - T1 第 4 步 `session_manager.delete_session().await?` — **会拒绝吗？** 取决于 SessionManager 实现，多半返回 Ok（idempotent）或 NotFound；
  - 结果：T2 的 bridge 进程在「已删除」的 session 上继续跑，事件流到一个不存在的 session_id，前端可能报错或忽略。
- **关联上下文：**
  - 用户层面：先点「删除」，再快速点「发送」（极快连击）会触发；
  - API 层面：两个 RPC 客户端同时请求；
  - delete 后 session_manager 也会 broadcast `SessionDeleted`，前端会清掉 UI；T2 的事件流到达时 router 找不到接收端，事件被丢——但 bridge 进程仍跑、仍计费。
- **重现/证明：** 写并发测试：`tokio::join!(coordinator.delete_session(...), coordinator.handle_user_input(...))`。
- **修复建议：** 把 `runtime_sessions` 操作都放到一个更高层的 `RwLock` 或 `Mutex<HashMap>` 下，**或者**让 `handle_user_input` 在创建/复用 session 之前先校验 session_manager 里是否仍存在：
  ```rust
  // handle_user_input runtime 分支顶部：
  if !self.session_manager.session_exists(&session_id) {
      return Err(BitFunError::Validation("session deleted".into()));
  }
  ```
  这只是缩小窗口、不能根除。根除需要在 SessionManager 里维护「是否 deleting」状态位，handle_user_input 看到 deleting 直接 reject。
- **参考：** CWE-362 Race Condition。

---

### P-7 🟡 中危 ｜ 文档-误导 ｜ `delete_session` 的 doc comment 关于「persistence-only purge」的描述不准确

- **位置：** `coordinator.rs:3415~3423`（commit `0d88f2f5` 加的注释）
- **详细描述：** 注释写：
  > Direct calls to `session_manager.delete_session` are acceptable only when the session is known not to be loaded in memory — i.e. there is no `runtime_sessions` or `active_turns_per_session` entry for it (e.g. the persistence-only purge in `session_api.rs`).
  
  但实际看 `session_api.rs:340~357` 的 `delete_persisted_session` 命令：它直接 `PersistenceManager::new(...).delete_session(...)`，**完全绕过了 coordinator 和 session_manager**。它访问的是文件系统层。所以注释里说「persistence-only purge」走的是 `session_manager.delete_session` 是错的，实际走的是 PersistenceManager。
  
  另外 `session_api.rs:584~592` 的 `delete_all_archived_sessions` 也直接走 PersistenceManager。**没有任何 session_api 调用 `session_manager.delete_session` 而绕开 coordinator** — 注释假设的「合法绕过」分支其实不存在。
- **关联上下文：** 真正的问题是另一种：如果用户先 `archive_session`（仅改 metadata.status）→ 然后 `unarchive_session`（再改回来）→ 然后 `delete_persisted_session`，期间这个 session 可能被其他 client（agent_session_api、bot_api）restore 加载到内存。delete_persisted_session 走的是 PersistenceManager，**不会通知 coordinator 清理 in-memory state** → 完全绕过 review1 P4 的修复，泄漏照旧。
- **修复建议：**
  1. 把注释改成准确陈述：「session_api.rs 的 `delete_persisted_session` 走 PersistenceManager 直接清磁盘，仅当 session 已知未被任何 coordinator 加载时安全；当前没有强制约束，未来需要加守卫。」
  2. 或者更彻底：让 `delete_persisted_session` 先调 `coordinator.delete_session`（若 session 在内存里），再清磁盘；coordinator.delete_session 已经会调 session_manager.delete_session，幂等。
- **参考：** Best practice — comments must describe actual code behavior.

---

### P-8 🔵 低危 ｜ 资源-bridge stderr 通道 ｜ `Stdio::inherit()` 把 SDK 日志混入主进程

- **位置：** `claude_runtime.rs:144`
- **详细描述：** `stderr(std::process::Stdio::inherit())` 让 bridge 的 stderr 直接写到 Rust 进程的 stderr。Tauri 桌面应用里这通常落到系统 console / log file，但：
  - SDK debug 模式下会打印 prompt 内容（敏感）、API URL、auth header 部分内容（依实现）；
  - 失败堆栈无结构化捕获，遥测看不到；
  - Windows 上 `Stdio::inherit` 共享 console buffer 可能导致并发写入交错。
- **修复建议：** 改成 `Stdio::piped()` + 单独 reader task → `log::warn!`/`log::debug!`，并 sanitize（去掉 API key 片段）。优先级低，不在 hotfix 范围。

---

### P-9 🔵 低危 ｜ 死代码 ｜ bridge.mjs 的 `abort` 命令路径

- **位置：** `bridge.mjs:197~199`，`claude_runtime.rs:319~329`
- **详细描述：** bridge 支持 `{"command":"abort"}` 命令，收到则 `process.exit(0)`。但 Rust 端的 `abort()` 实现走的是 `child.kill()`（SIGKILL），不通过 stdin 发 abort 命令。这段代码实际从未触发。
- **后果：** 没 bug，但维护成本（如果以后有人改 abort 协议会困惑）。
- **修复建议：** 要么改 `abort()` 实现走 graceful stdin（更优，能让 SDK 关闭 HTTP 连接），要么删掉 bridge 里的 abort handler。

---

### P-10 🔵 低危 ｜ 信息丢失 ｜ TurnEnd 非 `Completed` / `Aborted` 的消息只保留 Debug 字符串

- **位置：** `coordinator.rs:2763`
- **详细描述：** `let err_msg = format!("Runtime turn ended: {:?}", stop_reason);` — 把 `StopReason::Error` / 未来新增 variant 全部塞成 debug 字串。`classify_runtime_error` 对这种字串只能命中 `"error"`（无任何 pattern），落到 `ModelError`（fallback）。无法区分「provider 限流导致的 stop_reason」与「内容策略阻断导致的 stop_reason」。
- **修复建议：** 让 `StopReason::Error` 携带 reason payload（`StopReason::Error { message: String, code: Option<String> }`），或在 bridge 端把更详细的 stop reason 透传到 `turn_end` 事件，coordinator 再读出来。

---

### P-11 ⚪ 建议 ｜ `parseTimeoutMs` 不接受合法 ISO 数值字串

- **位置：** `bridge.mjs:32~35`
- **详细描述：** `parseInt('60s', 10) = 60` → 60 < 1000 → fallback；`parseInt('60_000', 10) = 60`（下划线分隔不识别）；`parseInt('1.5e5', 10) = 1`。容错性一般，但用户文档里只承诺 ms 整数，这是合理限制。
- **修复建议：** 用 `Number(raw)` 替代 `parseInt(raw, 10)`，并显式检查整数性 `Number.isInteger`。可选改进，非 bug。

---

### P-12 ⚪ 建议 ｜ bridge 错误消息直传前端，缺乏脱敏

- **位置：** `bridge.mjs:264~267`
- **详细描述：** `err.message ?? String(err)` 直接写入 `{type:'error', message}`，coordinator 直接放进 `DialogTurnFailed.error` 字段、event_queue 广播给前端。SDK 错误消息可能包含：
  - 完整 HTTP 请求 URL（含 query string）；
  - 部分 system prompt 片段（如果 SDK 在错误里 echo back）；
  - request_id、x-internal-* header（debug 模式）。
- **修复建议：** 把错误消息按 category 归类后再脱敏（保留 category + 通用文案 + provider_message 截断），原始消息只进结构化日志。

---

## 3. 可疑模式与潜在风险

1. **「runtime spawn task 不归 ExecutionEngine 管」是个根本性割裂。** bitfun 路径里 ExecutionEngine 是事实上的 turn lifecycle 总线（cancel token / has_active_turn / cleanup_cancel_token）。runtime 路径完全平行另搞一套（active_turns counter + runtime_sessions），但只复用了一半 cancel API（`cancel_active_turn_for_session` 只在 SessionManager state 层有效）。建议把 runtime turn 也注册到 ExecutionEngine（即使是个"空"实现，只为 cancel token 寄存），或者 SessionManager 内置一个 cancel-token registry，两条路径都接入。

2. **`active_counter` 与 `runtime_sessions` 是两个独立 DashMap，没原子化关联。** `delete_session` 分两步 remove，期间 `handle_user_input` 可以同时操作两者。后续若加新 per-session 字段（log buffer、metrics 等），又得加一个 DashMap 又得再补一处 delete_session。**重构方向：把所有 per-session coordinator-owned 状态收进一个 `RuntimeSessionEntry { session: Mutex<…>, cancel: CancellationToken, counter: AtomicUsize, … }`，DashMap key→Arc 一次拿全。**

3. **`classify_ai_error_message` 的契约文档缺失。** 函数签名/注释里没说「永不返回 Unknown」。这正是 P-1 的根因。建议加注释 `/// Returns ModelError as the fallback; never returns Unknown unless input is empty.` 或者直接改契约：「找不到匹配返回 Unknown」，让调用方自己决定 fallback。

4. **bridge.mjs 的 `for await (const line of rl)` 与新加的 inner timeout/throw 之间的栈展开没校对充分。** 单测覆盖 0。建议加 mocked stdin/stdout 的集成测试：
   - 正常 turn → events + turn_end completed
   - prompt 抛错 → error + turn_end error
   - first-event timeout → error + turn_end error，且能继续处理下一个 prompt 命令
   - idle timeout 同上
   - iter.return 自身抛错 → 不能吞掉原 timeout 错误

5. **`kill_on_drop(true)` 在 Windows 上行为略不同**（SIGTERM 在 Windows 上是 TerminateProcess）。Node bridge 的 child process 没有 cleanup hook，直接 TerminateProcess 会让 SDK 内的 in-flight HTTP 不能 graceful close → Anthropic 服务端会看到 abort 连接，对长 prompt 可能仍计费一部分 token。这是 SDK / 协议限制，但建议 dispose 时先 stdin write `'{"command":"abort"}\n'` 给 50 ms graceful 窗口、再 kill。

6. **环境变量名 `BITFUN_CLAUDE_BRIDGE_FIRST_EVENT_TIMEOUT_MS` 没在任何 doc/CHANGELOG 提及。** 看 commit message 写了，但运维 / 用户找不到。建议在 `AGENTS.md` 或 `docs/runtime-claude.md` 里加一节「Environment variables」。

---

## 4. 逻辑流追踪

### 4.1 用户取消正在跑的 runtime turn

```
User → cancel_dialog_turn(sid, tid)
  ├ session_manager.update_session_state_for_turn_if_processing → Idle ✓ (UI 立即看到)
  ├ execution_engine.cancel_dialog_turn  → no-op (runtime 没注册)
  ├ tool_pipeline.cancel_dialog_turn_tools → no-op
  ├ cancel_active_subagents_for_parent_turn → no-op
  └ wait_session_drained(1500ms) → 看到 counter=1，spin 1500ms → 超时 warn

[runtime spawn task 仍在跑] ←─────── 没收到任何取消信号
   ├ stream.next() 仍在阻塞
   ├ bridge 仍在调 Anthropic API
   └ 等 IDLE_TIMEOUT_MS(120s) 或自然 turn_end

User UI 看到「已停止」但实际 token 仍在烧 → P-2 的核心症状
```

### 4.2 删除运行中的 runtime session

```
delete_session
  ├ cancel_active_turn_for_session(2s)
  │   └ 见上，runtime 不响应取消
  ├ runtime_sessions.remove(sid)
  │   ├ slot.lock().await.take() — slot 当前是 None（spawn task 拿走了）
  │   └ 不调 dispose
  ├ active_turns_per_session.remove(sid) — 但 spawn task 仍持 active_counter Arc
  ├ session_manager.delete_session — 删持久化、删内存索引
  └ emit SessionDeleted

[runtime spawn task 继续跑]
   ├ 自然结束 → 走 put-back → slot 现在不在 DashMap，仅 spawn task 持 Arc
   ├ task return → Arc 引用归零 → Box<dyn AgentSession> drop
   └ kill_on_drop(true) → SIGKILL bridge ✓ 终于死了
```

> 救火链上的最后兜底是 `kill_on_drop`。这是 cb2832ae 的真正价值：补的不是 P5 part A 那条路径，而是 P-2 这条「delete_session 时碰巧有活跃 turn」的隐式 drop 路径。**值得单独记一笔：commit message 没说清这一点**。

### 4.3 bridge first-event timeout 后的栈展开

```
prompt() 进入 try{}
  ├ query() 同步返回 messages iterator
  ├ iter = messages[Symbol.asyncIterator]()
  ├ while(true) ── 第一轮 ──
  │   ├ Promise.race([iter.next(), timeoutPromise])
  │   ├ timeout 先 reject
  │   ├ 进入 inner catch
  │   │   ├ clearTimeout(timer) ✓
  │   │   ├ await iter.return?.() ← 可能 hang！P-4
  │   │   └ throw err
  │   └ [if iter.return 没 hang] 跳出 while
  ├ 进入 outer catch
  │   ├ stdout.write({type:'error', message}) ✓
  │   └ stdout.write({type:'turn_end', stopReason:'error'}) ✓
  └ 回到 for-await readline 循环等下一个 stdin 命令

[Rust spawn task]
   ├ stream.next() → RuntimeEvent::Error
   ├ break，进入 put-back ← P-5：bridge 还在 outer catch 块里没回到 readline，session 已经被塞回 cache
   └ 下次 prompt → write stdin → readline 已经返回到下一 iteration（如果没卡）→ 正常处理 ✓

[失败模式：iter.return hang]
   ├ outer try 一直没退出
   ├ readline 没 advance
   ├ 下次 prompt → stdin 缓冲增长
   └ 直到 hang 解除（可能永远）
```

---

## 5. 安全性专项审查（OWASP Top 10 2021）

| 类别 | 是否存在 | 说明 |
|------|---------|------|
| A01 Broken Access Control | 不适用 | 这一改动范围内无授权决策 |
| A02 Cryptographic Failures | 不涉及 | bridge 走 HTTPS（SDK 自管），ANTHROPIC_API_KEY 用环境变量传递，未持久化 |
| A03 Injection | ⚠️ 低 | `cmd_line = format!("{}\n", cmd)` 在 stdin 上，cmd 是 serde_json 序列化的，已转义；用户文本走 `text` 字段，bridge 端 `JSON.parse` 解码——无注入风险 |
| A04 Insecure Design | **🟠 中** | runtime 路径与 bitfun 路径的取消/清理不对称（P-2、P-3）属于设计不一致；异常路径处理不对称（P-5） |
| A05 Security Misconfiguration | ⚠️ 低 | bridge stderr inherit（P-8）可能在 Tauri 配置下泄露日志；timeout 默认 120 s 偏高，DoS 自助 |
| A06 Vulnerable Components | 未审 | `@anthropic-ai/claude-agent-sdk` 版本未在本次 review 范围内验证 |
| A07 Identification & Auth Failures | 不涉及 | API key 从环境变量读，未做 key rotation 设计——本次改动未影响 |
| A08 Data Integrity Failures | ⚠️ 低 | bridge.mjs 是从 npm 安装的 SDK，没固定 lock file（pnpm-lock.yaml 应已固化）；SDK 内部代码完整性依赖包管理器；本次改动不涉及 |
| A09 Logging & Monitoring | **🟡 中** | 错误分类失真（P-1）直接打击 monitoring 的可用性；错误消息脱敏缺失（P-12）违反「敏感信息日志最小化」 |
| A10 SSRF | 不适用 | bridge 不接受用户 URL，仅 HTTPS 到 Anthropic API |

**信任边界标注：**
1. **stdin (Rust → bridge)**：JSON 编码、长度无界。Node 端 `JSON.parse` 已是安全反序列化，但**没有 max-line-length 检查**——恶意/损坏的 stdin 可让 readline 内存膨胀。Rust 端是受信源，但建议加 1 MB 行长上限作为深度防御。
2. **stdout (bridge → Rust)**：JSON 编码、转译过的 SDK 消息。Rust 端 `serde_json::from_str::<Value>(&trimmed)`（`claude_runtime.rs:193`），失败仅静默丢弃——没问题，但失败计数应进 metrics。
3. **bridge stderr → Rust 进程 stderr (inherit)**：见 P-8。

---

## 6. 最终建议

### 必须修复（合并前）
1. **P-1 死代码错误分类** — 改一行守卫顺序。**风险/收益比极高**。
2. **P-3 panic 安全 RAII** — 不修就埋雷。改动量小（一个 struct + 三处替换 fetch_add/sub）。
3. **P-4 iter.return 限时** — 5 行代码。

### 强烈推荐（24 小时内）
4. **P-2 runtime turn 真取消** — 引入 CancellationToken。改动稍大（需要修改 entry 结构 + spawn task select），但是 Beta 必须项；当前状态下 ESC 取消、删除会话都是「假取消」。
5. **P-5 RuntimeEvent::Error 走 dispose 路径** — 几行代码，与 prompt() Err 对称。
6. **P-7 修正 doc comment** — 简单。

### 长期重构（下一轮迭代）
7. **统一 RuntimeSessionEntry 结构**（见可疑模式 §2）—— 把 active_counter / cancel_token / session_slot 收进一个 Arc，砍掉两个并行 DashMap 的同步成本。
8. **runtime turn 接入 ExecutionEngine** —— 把「cancel token registry」抽成 SessionManager 的能力，bitfun / runtime 都注册，一处取消、一处查询。
9. **P-8 / P-12 日志/错误脱敏** —— 配合 telemetry 改造一次性做。
10. **bridge 集成测试** —— 至少覆盖第 4 节列举的 5 个场景。

### 架构观察
- 这个 `runtime_id` 分发模式**正在双轨运行**：bitfun 路径是「ExecutionEngine 主导」，runtime 路径是「coordinator 直 spawn」。短期内可以共存，但 hotfix 暴露的所有问题（取消、清理、panic 安全、状态污染）都源于「runtime 路径绕开了 bitfun 路径精心搭建的中间层」。建议在 Beta 升级到 GA 之前，把 runtime 路径也搬到 ExecutionEngine 抽象之下，把 ExecutionEngine 重命名为更通用的 `TurnExecutor`，bitfun / claude / OMP 都是其中的一种 Strategy。这个改造一次到位，比每次 hotfix 都打一个补丁可持续得多。

---

## 附录 — 审查覆盖说明

- ✅ 5 个 hotfix commit 全部 diff 阅读完毕
- ✅ coordinator.rs runtime 分支 (`2580~2825`) 全程逐行
- ✅ delete_session (`3410~3467`)、cancel_dialog_turn (`3276~3372`)、cancel_active_turn_for_session (`3374~3408`)、wait_session_drained (`3171~3187`) 调用链
- ✅ classify_ai_error_message / ai_error_detail_from_message (`core-types/src/errors.rs`) 完整源码 + 既有单测
- ✅ ClaudeSession 全文（`claude_runtime.rs`）
- ✅ bridge.mjs 全文
- ✅ session_api.rs 删除/归档相关命令（`340~595`）
- ⚠️ 未审：`@anthropic-ai/claude-agent-sdk` 内部 iterator 实现（影响 P-4 严重程度的精确判断）
- ⚠️ 未审：Windows 平台 `kill_on_drop` 的精确时序（影响 P-2 兜底窗口的精确测算）
