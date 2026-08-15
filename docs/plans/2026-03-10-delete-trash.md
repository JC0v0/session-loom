# 删除会话与回收站

## 背景

原"删除镜像"只删 store 记录，源文件还在，守护进程下次扫描就把会话镜像回来，删除等于无效。改为真正的"删除会话"：同时删除镜像与源应用中的原始会话，并引入 30 天回收站兜底。

## 关键决策

- **删除语义**：删除 = 镜像行 + 源会话 + 回收站归档。顺序：先写入回收站（数据兜底），再删源会话（尽力而为，失败仅警告），最后删镜像行。
- **源会话删除 per 渠道**：Codex 删 rollout 文件（文件名按 -<sessionId>.jsonl 后缀搜索兜底）；Claude 删会话 jsonl 并重写 history.jsonl 去掉该条目；OpenCode 删 session/message/part 行；DSH 删会话目录（projectKey/encodeSegment 反推路径）。
- **source_path 列**：sessions 表新增 source_path 记录镜像来源文件；watcher 每次镜像时写入，旧库 ALTER TABLE 迁移；无记录时按各渠道命名规则反推。
- **回收站**：<store_root>/trash/<sessionId>.json 存 canonical 会话 + deletedAt + 来源；30 天过期清理（守护进程每小时 + 删除时顺带清理）。
- **墓碑**：watcher 跳过回收站中存在的 session id，删除后不会再被镜像回来；恢复时移出回收站后恢复正常镜像。
- **恢复去向（用户确认）**：回收站恢复只回到镜像列表；推回源应用仍走现有"恢复到 X"。

## 实现

- crates/session-loom-core/src/trash.rs：回收站存储/列表/恢复/清理。
- crates/session-loom-core/src/delete.rs：删除编排与四渠道源删除。
- store.rs source_path 列与迁移；watcher 传源路径 + 墓碑检查 + 定时清理；CLI 新增 delete 与 trash list/restore/delete；桌面端 sessions_delete 改语义 + trash_list/trash_restore/trash_delete 命令；UI 增加回收站视图。

## 修复记录

- **2026-03-10（滚动与查看）**：回收站列表无法滚动、条目无法查看。根因：`#trashView` 作为 main 网格子项，行高内容自适应，flex:1+overflow 在无确定高度时不生效。修复：回收站改为覆盖主区域的绝对定位层（position:absolute; inset:0），高度确定后列表必然可滚动；新增 `#trashDetailBody` 详情页（点击条目查看完整对话，复用抽取出的 renderConversation），支持详情→列表两级返回。

## 验证

- 43 个 workspace 测试通过（含新增 8 个 trash/delete 集成测试：源路径持久化与迁移、回收站往返与过期清理、四渠道删除、墓碑阻止重镜像）；fmt 与 clippy -D warnings 干净。
- 真机全链路：删除真实 DSH 会话 → 源目录删除、镜像删除、回收站入库、守护进程 5 秒后仍未重镜像；trash restore → 回到镜像列表且回收站清空；再恢复到 DSH 生成新会话。
- **2026-03-10（布局重构）**：覆盖层方案在 main 网格中高度仍不被约束（auto 行高），回收站列表依旧不可滚动。最终按"参考首页"重构：回收站不再独立成层，而是作为模式切换复用首页布局——侧栏显示回收站列表（与首页同一份 .card/滚动 CSS），详情区复用 #detailBody 显示对话，头部侧栏工具区出现"回收站"说明与返回按钮；3 秒刷新在回收站模式下改为刷新回收站。
