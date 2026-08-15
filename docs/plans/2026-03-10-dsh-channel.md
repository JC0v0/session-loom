# DeepSeek Harness 会话渠道

## 背景

新增 DeepSeek Harness（DSH）作为第四个镜像渠道，支持导入、监听与恢复。格式结论来自对 F:\deepseek-harness 源码（session-persistence-jsonl 后端、dsh-session 事件词汇与不变量、dsh-llm 消息结构）与本机实时会话日志的双向验证。
## 修复记录

- **2026-03-10**：恢复的会话在 DSH 继续对话时报 DeepSeek API 错误 "Messages with role 'tool' must be a response to a preceding message with 'tool_calls'"。根因：DSH 派生历史从 assistant/message 的 content 块重建工具调用（适配器据此生成请求的 tool_calls），首版恢复只写了独立 tool/call 事件、assistant 消息里没有 tool-call 块，导致请求里 tool 结果悬空。修复：render_events 在 assistant/message content 中嵌入与 tool/call/tool/result 同 id 的 tool-call 块（tool/call 事件保留以满足不变量）；无输出的工具结果补占位文本 "(output not captured)" 避免空 tool 消息。回归测试 dsh_restore_embeds_tool_call_blocks_in_assistant_messages 锁定该结构，并用真实 Codex 会话（215 消息/457 工具调用）恢复验证：103 个含工具的 assistant 消息全部带块、457 对 call/result id 全部匹配、无空结果。
