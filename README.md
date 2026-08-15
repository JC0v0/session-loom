# Session-Loom

把 **Claude Code** 和 **Codex** 的会话持续镜像到一份统一、带版本号的 canonical 格式中,并能将任意一份会话**恢复到另一个工具里原生续接**——不复制粘贴、不重新解释上下文。

Claude Code 和 Codex 各自使用私有的会话存储格式,官方都没有双向导入能力。Session-Loom 通过「镜像 → 统一格式 → 恢复」补上这块空白:

```
~/.codex/sessions ──┐
                    ├─→ 守护进程(轮询镜像)─→ canonical 会话(SQLite)─→ 按需恢复 ─→ 对端工具原生会话
~/.claude/projects ─┘
```

## 特性

- **双向会话迁移**:Codex 会话可恢复为 Claude Code 会话,反之亦然;恢复结果能被 `claude --resume` / `codex resume` 原生列出并续接。
- **持续镜像**:后台守护进程以 2 秒轮询监听两个工具的会话目录,会话一旦新增或更新即同步进 canonical 存储;镜像进程**只读不写**被监听目录,不会自回声。
- **canonical 中间格式**:会话统一为带 schema 版本的结构(sourceTool、sessionId、cwd、时间戳、有序消息与工具调用),持久化在 SQLite 中,为归档、搜索、跨机同步留好扩展点。
- **只迁移对话**:迁移用户/助手消息与工具调用记录(name、input、output 原样保留,**不重新执行**),丢弃源工具的系统提示词与 IDE 注入的上下文。
- **桌面应用**:Tauri 2 桌面客户端——会话浏览、全文搜索、按来源过滤、对话详情(含工具调用展开)、一键恢复到 Claude / Codex、删除镜像、守护进程开关。支持 Windows(NSIS 安装包)与 macOS(app / DMG)。
- **命令行 `ssl`**:`daemon` / `restore` / `list` / `search` / `export` 五个命令,纯文本输出,脚本友好。

## 架构

Rust workspace,三个 crate 共享一份领域逻辑:

```
session-loom/
├── crates/
│   ├── session-loom-core/     # 核心:canonical 模型、Codex/Claude 读写适配器、
│   │                          # SQLite 存储、恢复、轮询 watcher、守护进程生命周期
│   └── session-loom-cli/      # ssl 命令行(clap),只做参数解析,逻辑全部在 core
├── src-tauri/                 # Tauri 2 桌面壳,直接复用 session-loom-core;
│   └── binaries/              # 构建时由 scripts/prepare-rust-cli.cjs 放入 ssl 边车二进制
├── ui/                        # 桌面前端(原生 HTML/CSS/JS,无构建步骤;
│                              # 改 ui/theme.css 后运行 node scripts/inline-theme.cjs)
└── docs/plans/                # 设计与规划文档
```

守护进程镜像,CLI 和桌面端恢复;三者都落在同一份 canonical 存储上。

## 快速开始

环境要求:**Rust**(≥ 1.77.2,建议最新 stable)、**Node.js**(仅用于跑 npm 脚本和 Tauri CLI);Windows 桌面端依赖 WebView2。

```bash
git clone https://github.com/JC0v0/session-loom.git
cd session-loom
npm install
```

运行桌面应用(首次编译约 2 分钟):

```bash
npm run desktop
```

或直接使用 CLI:

```bash
npm start -- list                 # 等价于 cargo run -p session-loom-cli -- list
npm start -- daemon start         # 启动后台镜像守护进程
npm start -- restore --to codex   # 把最近的会话恢复到 Codex
```

测试与质量:

```bash
npm test                                   # = cargo test --workspace
cargo fmt --all -- --check                 # 格式检查
cargo clippy --workspace --all-targets -- -D warnings
```

打包发布:

```bash
npm run dist        # Windows:NSIS 安装包
npm run dist:mac    # macOS:.app 与 DMG
```

## CLI 用法

```text
ssl daemon [run|start|stop|status]     管理后台镜像守护进程(默认 start)
ssl restore --to <codex|claude> [id]   恢复会话到目标工具;省略 id 时取最近一条
ssl list [--tool <codex|claude>]       列出 canonical 会话
ssl search <关键词…>                   按内容或目录搜索会话
ssl export <id>                        导出一条会话的 canonical JSON
```

成功退出码为 0,运行失败为 1,用法错误为 2(clap)。

## 桌面应用

- 左侧:搜索框 + 全部 / Codex / Claude 标签页 + 会话卡片列表(标题、消息数、相对时间、工作目录)。
- 右侧:对话详情——逐条消息与可折叠的工具调用记录,一键「恢复到对端继续对话」「复制回本工具」「删除镜像」。
- 顶栏:守护进程运行状态,点击即可启动/停止镜像。

## 数据位置与环境变量

运行时数据全部存放在仓库之外:

| 内容 | 默认位置 | 覆盖变量 |
|---|---|---|
| canonical 存储(`sessions.db`、`daemon.pid`) | `~/.session-loom/` | `SESSION_LOOM_STORE`(兼容 `SESSION_BRIDGE_STORE`) |
| Codex 会话监听目录 | `~/.codex/sessions` | `CODEX_SESSIONS_ROOT` |
| Claude 会话监听目录 / 恢复根目录 | `~/.claude/projects` / `~/.claude` | `CLAUDE_ROOT` |

> 注意:会话数据库、对话内容属于本地隐私数据,请勿提交到仓库。

## 设计原则

- **统一中间格式,而非两两直连转换**:新增工具只需一对读写适配器。
- **镜像与恢复分离**:守护进程只镜像,恢复是显式的按需动作,避免把目标目录写回造成的回声循环。
- **结构保真,不做语义重放**:工具调用按记录保留,恢复到对端后不重新执行。
- **只搬对话,不搬提示词**:目标工具使用自己的系统提示词继续任务。

## 已知局限

- 两个工具的会话格式均为**私有且未文档化**,工具升级可能改变格式,需要跟进适配。
- 仅支持**同一台机器**上的迁移;跨机同步、OpenCode 适配、统一归档检索等已在规划(见 `docs/plans/`),暂未实现。

## 文档

- 原始设计计划:[docs/plans/2026-08-14-1409-feat-session-bridge-plan.md](docs/plans/2026-08-14-1409-feat-session-bridge-plan.md)
- 贡献指南与仓库约定:[AGENTS.md](AGENTS.md)
