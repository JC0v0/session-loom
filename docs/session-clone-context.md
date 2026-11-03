# 会话复刻的附带内容

桌面端沿用原有的“复刻到其他工具”入口。点击目标工具后，弹窗允许用户按需带上该会话所在项目的记忆与工具，并在写入前展示差异和逐项可用性。不再提供独立的“迁移项目”面板。
本文件是实现的兼容矩阵、使用说明与验证证据。

## 范围

一次复刻包含：

1. 用户点击的原生会话实例（不是合并卡片，不会自动带入未选 fork）。
2. 按需勾选的项目内容：指令与记忆、技能及资源树、MCP/外部连接、脚本与命令。
3. 按需勾选的全局或 profile 级内容。

所有附带内容都可留空；不勾选时只复刻会话。复刻使用当前来源快照，不建立来源与目标之间的持续同步；复刻后来源内容保持不变。

## 流程

```text
在会话详情点击“复刻到 <目标工具>”
  → 发现当前会话所属项目的可附带内容
  → 按需勾选项目/全局内容（可全不选）
  → 按目标原生格式生成预览
  → 展示目标差异与冲突决定
  → 执行并逐项报告可用性
```

预览会固定来源快照、目标原值、冲突决定和最终字节。执行只接受当前预览；目标、快照或预览内容变化后会要求刷新。
执行前会先固定目标会话 ID、目标 cwd 以及 Codex rollout 路径等与时间相关的路径，中断后重试复用同一目标，不产生重复副本。
同一物理项目内的复刻会沿用原会话的 cwd 表示，避免侧栏因路径别名分成不同项目。
会话名称同样按目标渠道的原生字段写入，目标工具列表里显示的仍是原会话的名字：

| 渠道 | 原生标题字段 | 读取来源 |
|---|---|---|
| Codex | `session_index.jsonl` 的 `thread_name`；线程表 `title` | 索引最后一条同名记录 |
| Claude Code | `{"type":"custom-title","customTitle":…}` 记录（与 `/rename`、`--name` 相同） | 优先 `custom-title`，其次 `ai-title`，最后压缩 `summary` |
| OpenCode | `session.title` | 数据库标题列 |
| DSH | `session/title` 事件（`source.kind = "user"`，与重命名命令一致） | 日志中最后一条标题事件 |
| Pi | `session_info` 条目（与 `--name` 相同） | 文件顺序中最后一条非空名称 |

未提供标题时，各渠道回退到首条用户消息，保持原有行为。

## 来源发现

| 渠道 | 记忆/指令 | 技能与资源 | 外部连接 | 脚本/命令 |
|---|---|---|---|---|
| Codex | 项目/用户 `AGENTS.md`、覆盖文件、生成记忆 | `.agents/skills`、用户技能目录 | 用户或受信任项目的 `mcp_servers` | 项目脚本、用户 prompts |
| Claude Code | `CLAUDE.md`、规则、导入文件、项目 auto memory（含项目外自定义目录） | `.claude/skills`、命令文件及资源 | 项目 `.mcp.json`、用户配置中的全局/项目连接 | 项目脚本、`.claude/commands` |
| OpenCode | `AGENTS.md`、全局规则、配置 instructions | `.opencode/skills` 及兼容技能目录 | `mcp`（V1）或 `mcp.servers`（V2） | `command`（V1）或 `commands`（V2）、项目脚本 |
| DSH | 项目/用户 AGENTS/CLAUDE 指令链、显式记忆资源 | `.dsh/skills`、`.agents/skills`、用户技能 | profile Cordis patch 中的 MCP client 插件 | 项目脚本；插件型命令按能力判断 |
| Pi | 用户/项目指令链、项目配置引用 | `.pi/skills`、`.agents/skills`、用户技能 | 用户/项目 `mcp.json` 的 `mcpServers` | `.pi/prompts`、项目脚本 |

发现器保留来源优先级、作用域、使用证据和诊断。被更高优先级定义遮蔽的项不会自动选中；没有使用证据的全局项只作为候选，默认不选。

## 目标渲染

| 内容 | Codex | Claude Code | OpenCode | DSH | Pi |
|---|---|---|---|---|---|
| 指令/记忆 | `AGENTS.md` 管理段 + 持久副本 | `CLAUDE.md` 管理段 + 导入改写 | `AGENTS.md` | `AGENTS.md` | `AGENTS.md` |
| 技能与资源 | `.agents/skills` | `.claude/skills` | `.opencode/skills` | `.dsh/skills` | `.pi/skills` |
| MCP | 用户 `config.toml` 的 `mcp_servers` | 项目 `.mcp.json` 或用户配置 | `opencode.jsonc`（V1/V2 分别生成） | profile `cordis.patch.yml`（stdio） | `.pi/mcp.json` |
| 命令/脚本 | 用户 prompts；项目命令保留为只读副本 | `.claude/commands` | `command`/`commands` 配置 | 无等价命令时保留副本并标注不支持 | `.pi/prompts` |
| 脚本 | 复制入口与配套资源 | 同左 | 同左 | 同左 | 同左 |

配置编辑保留 JSONC 注释、TOML 无关字段、YAML 标签和表达式原文，并在生成后重新解析验证。
不执行脚本、不安装依赖、不连接外部服务、不复制登录/OAuth 状态。

### 版本差异

- OpenCode V1 与 V2 的 MCP 和命令命名空间不同；未知版本不会被猜成某一种。
- Pi 原生 MCP 需要目标版本支持；不支持时明确报告，不会写入无效配置。
- DSH 的 MCP 需要安装与 DSH 运行时版本匹配的 `@deepseek-ai/dsh-mcp-client`；profile 范围不等于单个项目。
- Codex 项目级 MCP 会落到用户配置并标注实际生效范围。
- 技能调用控制字段、命令参数语法或插件处理器无法等价表达时，会保留原文并标注需要核对或暂不支持。

## 可用性

每个迁移项分别报告：

| 结果 | 含义 |
|---|---|
| `available` | 目标原生格式与加载入口已验证，必要资源齐全 |
| `needsConfiguration` | 缺少依赖、变量、授权、项目 trust、启用步骤，或连接尚未验证 |
| `unsupported` | 目标版本没有等价表达，或无法在保留语义的条件下转换 |
| `failed` | 读取、写入、登记或校验失败 |

文件存在不等于可用。MCP 连接、脚本运行依赖和项目 trust 默认保持“需要补充配置”，直到有目标侧证据。
选择“保留目标内容”时，可用性依据目标现有内容判断，并标记为待验证。

## CLI

```bash
ssl migrate projects [--json]
ssl migrate prepare --project <项目 ID 或绝对路径> --session <原生会话 ID>… [--dsh-profile <name>] [--json]
ssl migrate capture --run <run-id> [--asset <全局项 ID>…] [--json]
ssl migrate preview --run <run-id> --to <codex|claude|opencode|dsh|pi> \
  --project-root <绝对路径> [--decisions <JSON 文件|->] [--opencode-version auto|v1|v2|unknown] \
  [--pi-mcp supported|unsupported|unknown] [--project-trusted] [--env NAME]… [--json]
ssl migrate apply --run <run-id> [--preview <preview-id>] [--json]
ssl migrate status --run <run-id> [--json]
```

- `prepare` 只发现并创建草稿，不写目标。
- `capture` 捕获项目默认项与显式勾选的全局项。
- `preview` 返回差异、冲突和最终文件摘要；决定通过 `--decisions` 提交。
- `apply` 接受当前预览；部分完成或失败时退出码为 `1`，结构化结果仍会输出，可重试同一预览恢复。
- 参数用法错误退出码为 `2`。

## 桌面

在会话详情点击“复刻到 <目标工具>”按钮，弹窗直接复用当前会话和原工作目录：

1. **选择附带内容**：项目内容按需勾选，全局内容折叠且默认不带；可全不选，仅复刻会话。
2. **确认差异**：同名或冲突内容展示目标差异，逐项选择保留、替换或手动合并。
3. **查看结果**：会话与附带内容分别报告可用、需要配置、不支持或失败；部分完成可重试未完成项。

关闭对话框不会撤销已写入内容；重新打开会从运行记录恢复结果、未完成项或预览。
截图见 `docs/assets/session-clone/`（隔离演示数据，不含真实用户内容）。

CLI 仍保留 `ssl migrate` 命令，供自动化、批量选择项目和脚本调用；桌面端不再暴露独立的项目迁移入口。

## 安全与恢复

- 运行记录、快照、预览和备份位于运行时 store 的 `migrations/<run-id>/`，使用私有权限。
- 目标路径必须位于已选项目或用户范围内；拒绝目录穿越、链接绕出、来源与目标重叠时的内容修改。
- 文件使用同目录临时文件与原子替换；覆盖前保留私有备份。
- 原生会话、索引登记和 canonical 关联分别记账；固定 ID 使重试不会创建第二个副本。
- 已完成运行的快照/备份可按保留窗口清理，不触碰目标文件和未完成任务。
- 凭据文件、内联密钥、URL/Header/参数中的敏感值不会被复制；差异中脱敏，脱敏文本不能作为最终内容。

## 验证证据

验证日期：2026-11-03。所有验证使用临时目录、隔离 HOME/工具根和合成数据。

| 检查 | 命令 | 结果 |
|---|---|---|
| 核心测试 | `cargo test -p session-loom-core` | 19 单元 + 124 项集成通过 |
| CLI 测试 | `cargo test -p session-loom-cli` | 8 项通过 |
| Tauri 测试 | `cargo test --workspace` | 6 项通过 |
| UI 状态测试 | `node --test ui/tests/session-clone.test.mjs` | 14 项通过 |
| 格式 | `cargo fmt --all -- --check` | 通过 |
| 静态检查 | `cargo clippy --workspace --all-targets -- -D warnings` | 通过 |
| 工作区回归 | `cargo test --workspace` | 通过 |

五渠道原生验证（`DSH_BIN=<dsh> cargo test -p session-loom-core --test project_environment_targets -- --ignored`，8 项通过）：

| 渠道 | 固定版本 | 原生证据 |
|---|---|---|
| Codex | 0.166.0 | `codex debug prompt-input` 读取生成的指令与技能；`mcp list --json` 解析生成的连接 |
| Claude Code | 2.1.316 | 本地 Anthropic 桩请求包含生成的指令、技能与 MCP 工具；`--mcp-config` 连接本地 stdio MCP 服务 |
| OpenCode | 1.18.34 | `debug skill` 列出生成技能；V1 `mcp list` 连接本地服务 |
| Pi | 1.1.0 | RPC `get_commands` 列出生成技能；`mcp list --json` 连接本地服务并发现工具 |
| DSH | 0.2.1-rc.5 | `--dump-config` 组合生成的 profile patch；`--session-id` 原生恢复生成的会话；启动时 MCP 客户端连接本地服务 |

桌面验证：隔离 store/HOME 下从会话详情的“复刻到 Claude”打开弹窗，仅勾选记忆和技能，处理同名技能差异，完成复刻并查看部分结果；未勾选的连接、脚本和命令没有进入目标。截图位于 `docs/assets/session-clone/`。

## 已知边界

- DSH 的指令/技能未做模型请求级探测；profile 加载、会话恢复和 MCP 连接已原生验证。
- DSH MCP 需要与运行时匹配的客户端插件版本。
- 依赖安装、服务授权、登录状态和远程 MCP 连接不会自动迁移或验证，按项报告后续操作。
- 迁移不会重新执行历史工具调用，也不会自动安装缺失软件。
