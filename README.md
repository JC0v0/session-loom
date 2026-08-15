# Session-Loom

把 **Claude Code**、**Codex**、**OpenCode** 和 **DeepSeek Harness** 的会话持续镜像到一份统一、带版本号的 canonical 格式中,并能将任意一份会话**恢复到另一个工具里原生续接**——不复制粘贴、不重新解释上下文。

这四个工具各自使用私有的会话存储格式,官方都没有双向导入能力。Session-Loom 通过「镜像 → 统一格式 → 恢复」补上这块空白:

```
~/.codex/sessions ────────┐
~/.claude/projects ───────┤
opencode 数据库 ───────────┼─→ 守护进程(轮询镜像)─→ canonical 会话(SQLite)─→ 按需恢复 ─→ 对端工具原生会话
~/.dsh/sessions ──────────┘
```

## 特性

- **多向会话迁移**:Codex 会话可恢复为 Claude Code、OpenCode 或 DeepSeek Harness 会话,反之亦然;恢复结果能被 `claude --resume` / `codex resume` 及 OpenCode、DSH 各自的会话列表原生列出并续接。
- **持续镜像**:后台守护进程以 2 秒轮询监听四个渠道的会话目录/数据库,会话一旦新增或更新即同步进 canonical 存储;镜像进程**只读不写**被监听数据,不会自回声。
- **canonical 中间格式**:会话统一为带 schema 版本的结构(sourceTool、sessionId、cwd、时间戳、有序消息与工具调用),持久化在 SQLite 中,为归档、搜索、跨机同步留好扩展点。
- **只迁移对话**:迁移用户/助手消息与工具调用记录(name、input、output 原样保留,**不重新执行**),丢弃源工具的系统提示词与 IDE 注入的上下文。
- **真删除与回收站**:删除会话会尽力删除原始会话文件,并把镜像归档到 30 天回收站;回收站内可恢复或彻底删除,墓碑机制保证被删会话不会被守护进程重新镜像回来。
- **桌面应用**:Tauri 2 桌面客户端——会话浏览、全文搜索、按来源过滤、对话详情(含工具调用展开)、一键恢复到其他工具、删除会话、回收站视图、守护进程开关。支持 Windows(NSIS 安装包)与 macOS(app / DMG)。
- **命令行 `ssl`**:`daemon` / `restore` / `list` / `search` / `export` / `delete` / `trash` 七个命令,纯文本输出,脚本友好。Windows 安装包会附带 `ssl` 并自动加入当前用户 PATH,装完应用即可在终端直接使用 `ssl`。

## 架构

Rust workspace,三个 crate 共享一份领域逻辑:

```
session-loom/
├── crates/
│   ├── session-loom-core/     # 核心:canonical 模型、四渠道读写适配器、
│   │                          # SQLite 存储、恢复、轮询 watcher、删除/回收站、守护进程生命周期
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
npm start -- trash list           # 查看回收站
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
ssl daemon [run|start|stop|status]       管理后台镜像守护进程(默认 start)
ssl restore --to <codex|claude|opencode|dsh> [id]  恢复会话到目标工具;省略 id 时取最近一条
ssl list [--tool <codex|claude|opencode|dsh>]      列出 canonical 会话
ssl search <关键词…>                     按内容或目录搜索会话
ssl export <id>                          导出一条会话的 canonical JSON
ssl delete <id>                          删除会话(镜像 + 原始会话),归档进 30 天回收站
ssl trash list                           列出回收站条目
ssl trash restore <id>                   把回收站中的会话恢复到镜像列表
ssl trash delete <id>                    彻底删除回收站中的会话(不可恢复)
```

成功退出码为 0,运行失败为 1,用法错误为 2(clap)。

## 桌面应用

- 左侧:搜索框 + 全部 / Codex / Claude / OpenCode / DSH 标签页 + 会话卡片列表(标题、消息数、相对时间、工作目录);「回收站」按钮进入回收站模式,列出已删除会话并可恢复或彻底删除。
- 右侧:对话详情——逐条消息与可折叠的工具调用记录,一键「恢复到其他工具继续对话」「复制回本工具」「删除会话(移入回收站)」。
- 顶栏:守护进程运行状态,点击即可启动/停止镜像。

## 数据位置与环境变量

运行时数据全部存放在仓库之外:

| 内容 | 默认位置 | 覆盖变量 |
|---|---|---|
| canonical 存储(`sessions.db`、`daemon.pid`) | `~/.session-loom/` | `SESSION_LOOM_STORE`(兼容 `SESSION_BRIDGE_STORE`) |
| 回收站(被删会话的归档) | `~/.session-loom/trash/` | 随 `SESSION_LOOM_STORE` |
| Codex 会话监听目录 | `~/.codex/sessions` | `CODEX_SESSIONS_ROOT` |
| Claude 会话监听目录 / 恢复根目录 | `~/.claude/projects` / `~/.claude` | `CLAUDE_ROOT` |
| OpenCode 数据库 / 数据目录 | `<data>/opencode/opencode.db` | `OPENCODE_DB`、`OPENCODE_DATA_DIR`(兼容 `XDG_DATA_HOME`) |
| DeepSeek Harness 会话监听/恢复根目录 | `~/.dsh/sessions` | `DSH_SESSIONS_ROOT`(兼容 `DSH_HOME`) |

> 注意:会话数据库、对话内容属于本地隐私数据,请勿提交到仓库。

## 设计原则

- **统一中间格式,而非两两直连转换**:新增工具只需一对读写适配器。
- **镜像与恢复分离**:守护进程只镜像,恢复是显式的按需动作,避免把目标目录写回造成的回声循环。
- **结构保真,不做语义重放**:工具调用按记录保留,恢复到对端后不重新执行。
- **只搬对话,不搬提示词**:目标工具使用自己的系统提示词继续任务。
- **删除可回退**:真删除前先把镜像归档进 30 天回收站;源会话删除失败不影响镜像删除,数据始终有兜底。

## 已知局限

- 各工具的会话格式均为**私有且未文档化**,工具升级可能改变格式,需要跟进适配。
- 仅支持**同一台机器**上的迁移;跨机同步、统一归档检索等已在规划(见 `docs/plans/`),暂未实现。

## 文档

- 原始设计计划:[docs/plans/2026-08-14-1409-feat-session-bridge-plan.md](docs/plans/2026-08-14-1409-feat-session-bridge-plan.md)
- 贡献指南与仓库约定:[AGENTS.md](AGENTS.md)
