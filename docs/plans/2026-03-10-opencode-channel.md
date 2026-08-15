# OpenCode 会话渠道

## 背景

Session-Loom 现有 Codex 与 Claude 两个会话镜像渠道，需要新增 OpenCode（sst/opencode，现托管于 anomalyco/opencode）作为第三个渠道，支持导入、监听与恢复。

## 关键决策

- **对接新版 SQLite 存储，不做旧版 JSON 布局兼容**：OpenCode v1.18.x 起会话持久化在 <data>/opencode.db（session / message / part / project 表，message 与 part 的 data 列保存 V1 结构的 JSON），storage/ 目录下的多文件 JSON 只是迁移前的遗留格式，仅作为旧数据保留。仓库已有 rusqlite 依赖，直接读库即可。
- **监听单元是数据库文件本身**：OpenCode 每次会话更新都会改写 opencode.db，watcher 沿用 mtime+size 签名去重，扫描时把库内全部会话镜像进 store；Codex/Claude 的 .jsonl 逐文件监听逻辑保持不变。
- **恢复写入同一数据库**：生成 ses_/msg_/prt_ 前缀 ID 与 V1 结构 JSON；project 解析按 worktree/project_directory 复用 → git 根提交哈希 → "global" 的顺序兜底。OpenCode 打开目录时会把匹配的会话按 directory 自动归户到解析出的 project，因此 projectID 推导即使不完全一致也会被自行修正。
- **环境变量**：OPENCODE_DB（绝对路径或相对 data 目录）、OPENCODE_DATA_DIR、XDG_DATA_HOME；默认 <home>/.local/share/opencode/opencode.db，缺失时回退匹配 opencode-*.db（频道版数据库）。

## 实现

- crates/session-loom-core/src/adapters/opencode.rs：parse_sessions（导入）与 write_session_to_database（恢复）。
- canonical.rs 增加 SourceTool::OpenCode；paths.rs 增加 opencode_data_dir / opencode_database。
- watcher.rs 对 OpenCode target 监听 db 文件；restore.rs、CLI、桌面 UI 均增加 opencode 分支。

## 验证

- 适配器往返测试（写入后解析比对消息与工具调用）、手工建库解析测试、watcher 监听 db 变更测试、restore 恢复后回读验证。
- cargo fmt --all -- --check、cargo clippy --workspace --all-targets -- -D warnings、cargo test --workspace 全部通过（29 个测试）。
