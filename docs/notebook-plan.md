# PeerCarry 笔记本第一版实现约定

日期：2026-10-27。用户要求增加类似苹果备忘录的笔记本，包含 Markdown 笔记、独立 Todo 清单和各设备剪贴板，并明确选择笔记和待办自动跨设备同步。此文件是本轮实现边界和前后端共享约定；本地版候选已交付，自动同步新增范围尚未验收。

主会话本轮已复核和集成本机候选，修正四处编辑保护问题并执行统一回归；仍需完成浏览器和自动交换运行链路收尾。实现及验收现状以 [集成复核记录](notebook-integration.md) 为准，下面的条目是目标约定，不表示每项已经验收。

## 第一版体验

桌面主导航调整为“笔记 / 待办 / 设备剪贴板”，传输记录、AI 提醒、连接手机及设置继续保留在次级导航。笔记和待办采用侧栏、条目列表、编辑区的布局，参考 [Apple Notes 的列表及内容区组织](https://support.apple.com/en-ie/guide/notes/apd8b73d28be/mac)，沿用 PeerCarry 暖白与青绿配色和系统中英文规则。

- Markdown：新建、标题/正文编辑、自动保存、编辑/预览切换、搜索、置顶、导出 .md、回收站和恢复。
- Todo：多个命名清单；新增、编辑、勾选、取消勾选和删除单项；展示完成数量；清单整体自动保存、搜索、置顶、回收站及恢复，导出 Markdown checklist。
- 设备剪贴板：保留现有历史和发送/复制/下载操作，增加来源设备筛选。列表是已采集历史，不能称为实时监听；“获取当前剪贴板”仍需显式点击并受对端权限控制，获取后只入历史。
- 第一版默认从“笔记”进入。短窗口提供列表/编辑区切换，不强行挤出三个窄列；键盘焦点、输入法组合输入和未保存内容不能被后台刷新打断。

用户已确认**笔记和待办自动跨设备同步**，覆盖运行新版 PeerCarry 且完成同步授权的桌面设备。本地自动保存和离线可用作为基础；自动同步使用独立通道，规则见下方“自动同步追加约定”。原先的“仅本地保存加手动分享”不再作为完整交付范围。剪贴板仍手动采集/取用，显式发送笔记文本作为可选操作保留。

第一版不扩展实时协同光标、附件/远程图片、iCloud 接入、密码库、通知/周期待办、日历和智能文件夹；本轮仍为桌面端，手机笔记界面尚未实现。

## 数据边界

笔记数据与剪贴板历史使用同一 Store 数据库中的独立 redb 表。普通采集、历史清理和 prune 都不能创建、修改或删除笔记。启动时以新增表兼容现有数据库，不迁移或重建原条目；笔记不进入现有 peer list/push、AI 事件或移动配对服务。

数据在各设备本地持久化，并仅在已授权的同步设备之间自动交换。第一版桌面传输依赖已连接 Tailscale 的加密链路和已配置的服务认证，不额外宣称存储加密或账号库级别的端到端密码系统。普通标题和正文不可写入 tracing/错误日志。客户端 localStorage 只保存视图偏好和选择 ID，不保存正文、任务文本或令牌。

NotebookItem 字段固定为：

```json
{
  "id": "server-generated-uuid",
  "kind": "markdown",
  "title": "会议记录",
  "markdown": "# 会议记录\n正文",
  "tasks": [],
  "pinned": false,
  "trashed": false,
  "revision": 1,
  "created_at": "2026-10-27T00:00:00Z",
  "updated_at": "2026-10-27T00:00:00Z"
}
```

kind 只能为 markdown 或 todo，创建后不可改变。Todo 的 markdown 固定为空，tasks 使用 `{ "id": "client-generated-uuid", "text": "任务", "done": false }`；Markdown 的 tasks 固定为空。id/时间/版本由服务端控制，任务 ID 仅用于编辑稳定性，不能作为权限凭据。请求拒绝未知字段。

限制：标题最多 200 个 Unicode 字符；单项任务文本最多 4096 字节，最多 500 项且 ID 不重复；单条完整记录 JSON 最大 256 KiB，全部笔记（含回收站）最多 1000 条和 32 MiB；请求 body 最大 512 KiB。到限额明确报错，不能自动清理或覆盖旧笔记。

所有修改在 redb 原子事务内完成。更新、回收、恢复使用 expected_revision 比对；过期版本返回 409，不按最后写入时间静默覆盖。reopen、修改失败、冲突、容量超限和历史清理隔离必须有回归。

## 本机 HTTP 约定

本节的编辑/管理 notebook API 必须同时验证：ConnectInfo 为 loopback，Host 是 localhost 或回环 IP，若有 Origin 必须与当前 http://Host 一致，若有 Sec-Fetch-Site 则只允许 same-origin 或 none。即使 allow_remote_apply=true 或提供有效 peer token，也不能从网络对端访问这些本机编辑接口。自动同步仅使用下方单独定义的 notebook-sync 只读网络端点，不放开现有本机 API。

本机页面通过已有 auth_middleware 的显式桌面白名单访问这些路径；每个笔记路由仍单独使用上述严格门禁。不要把普通 local_only 当作私密边界，因为它允许 allow_remote_apply 放宽访问。新资产同样保持桌面本机访问，不把认证令牌写进页面。

| 请求 | 数据及响应 |
| --- | --- |
| GET /v1/notebook?kind=markdown&q=...&trash=false | `{ "items": [NotebookSummary] }`；kind 可省略、markdown 或 todo；trash 默认 false；q 匹配标题与内容；置顶优先再按更新时间降序，稳定 id 排序；最多返回 1000 条摘要 |
| POST /v1/notebook | `{kind,title,markdown,tasks,pinned}`；正文/任务/pinned 可用空值默认；201 返回完整 NotebookItem；标题可为空表示未命名，由界面本地化显示 |
| GET /v1/notebook/{id} | 返回完整 NotebookItem；不存在 404，含已回收记录 |
| PUT /v1/notebook/{id} | `{kind,title,markdown,tasks,pinned,expected_revision}`；200 返回完整新版本；已回收记录不可编辑；kind 不可切换 |
| DELETE /v1/notebook/{id} | JSON `{expected_revision}`；只进入回收站，200 返回完整新版本；不永久删除 |
| POST /v1/notebook/{id}/restore | `{expected_revision}`；恢复并返回完整新版本 |
| POST /v1/notebook/{id}/publish | `{expected_revision}`；用户显式点击“发送”后，将已保存的当前版本导出为本机普通 text Entry，200 返回 `{entry_id}`；不读写实际剪贴板，不发送网络；已回收记录不可导出 |

NotebookSummary：`id,kind,title,preview,pinned,trashed,revision,created_at,updated_at,task_count,completed_count`。preview 最多 160 个字符且单行。列表不携带完整正文和任务。保存和列表搜索不创建普通历史条目。

400 用于非法参数/ID/类型；404 不存在；409 expected_revision 冲突；413 单条/request 过大；容量超限使用 409 和 error=quota_exceeded；500 只返回不含正文的通用存储失败消息。结构化错误统一 `{ "error": "revision_conflict", "message": "..." }`，revision_conflict 可带 current_revision；403 门禁可返回普通文本，前端需容错。

publish 是唯一将笔记转换为普通剪贴板历史的操作。前端在用户选择精确目标 node_id 后，先成功保存，再 publish，然后 POST /v1/transfers `{operation:"send",node_id,entry_id,expires_in_secs:86400}`。不使用“发送当前剪贴板”。队列发送失败时说明导出已进入本机历史、发送失败；不谎报送达。离线目标沿用现有队列能力。笔记正常保存和自动同步都不发布，文本不会因此进入普通剪贴板历史。

publish 对 Markdown 输出标题和原正文，对 Todo 输出标题和 `- [ ]` / `- [x]` 行；导出 .md 使用同样内容。前端可按文档字段生成下载，不允许向服务端传任意磁盘路径。

## 编辑和预览要求

- 自动保存请求串行执行，保留输入期间的最新内容；显示保存中/已保存/失败/冲突。列表选择、tab 切换、新建、回收、发送前处理未完成保存，失败时不能静默丢稿。
- 输入法 composing 期间不触发快捷键保存/切换；Ctrl/Cmd+S 支持显式保存。409 时保留本机编辑缓冲，提供重新载入或另存副本，不能自动用远端值覆盖输入。
- 预览支持标题、段落、粗体、斜体、行内/围栏代码、列表、引用、安全链接等常用 Markdown 子集；以 DOM/textContent 构建，禁止直接将原 Markdown 或 HTML 赋给 innerHTML，不加载外部脚本或 CDN。
- 原始 HTML、script、SVG、事件属性作为文字处理。链接仅 http/https，noopener/noreferrer；javascript/data 等不能成为可点击地址。不自动加载远程图片或资源。明确子集预览不等于完整 CommonMark 实现，原文和 .md 导出必须保真。
- Todo checkbox 改变待办状态，不调用剪贴板 apply；设备筛选和选择笔记也没有复制、采集或发送副作用。

## 文件分工

A 负责桌面完整交互：`crates/peercarry-core/timeline.html`、新增 `crates/peercarry-core/notebook.js`、`crates/peercarry-core/notebook.css`、`scripts/tests/notebook.test.cjs`，必要时窄改 `scripts/tests/workbench.test.cjs` 与 `ui-locale.test.cjs` 以保留原断言。只读后端及本文件；不改 Rust、Cargo 或手机端。Notebook JS/CSS 由 B 按下列路径嵌入服务：`/assets/notebook.js`、`/assets/notebook.css`。

B 负责本地数据完整链路：新增 `crates/peercarry-core/src/notebook.rs`，窄改 `store.rs`、`lib.rs`、`server.rs`，新增 `crates/peercarry-core/tests/notebook.rs`。可以在 notebook.rs 中构建 Router<AppState>，并在 server.rs 合并，具体内部拆分由 B 在上述边界内决定。禁止改 timeline、JS/CSS、Cargo、既有协议/配置、Flutter 或其他模块。

两个会话保持原生 ID，模型按用户默认 Luna。上一轮准备报告在安全边界收尾并释放旧 claim 后，用新目标/自己的精确路径重新 claim；本文件优先于旧的“仅写准备报告”限制。A/B 的范围互不重叠，根会话负责复核实际差异、边界和集成，不委派架构或授权决定。

主会话路径为本文件、会话安排、SESSION_INDEX 和 AGENTS 中的分工说明，不在两名执行会话编辑期间改其产品文件。共享 lockfile 不需要更新，不引入新依赖，沿用同一 primary checkout/main。

## 验收与交付

- B：持久化重开、CRUD/回收恢复、CAS 冲突、非法/超量数据、清理历史不影响笔记、无自动发布、publish 不触碰实际剪贴板。HTTP 测试覆盖正常 loopback、外部来源、伪造 Host/Origin/Sec-Fetch-Site、有效 token 与 allow_remote_apply=true 仍拒绝网络请求。
- A：真实行为测试覆盖新建/编辑/预览/Todo/筛选、保存时切换和失败、版本冲突与另存副本、空/错误状态、恶意 Markdown、原始导出、分享精确目标和防重复提交；保留工作台、语言、快捷面板及传输原有测试。
- 运行测试/构建时使用 target 下隔离数据和 TEMP/TMP，不启动读取真实数据的默认 daemon。B 的 include_str 依赖 A 的新资产，缺文件时明确依赖，不能用永久空资产绕过验证。
- 浏览器验收至少覆盖 940×680 与 560×420、中英文、笔记与待办闭环；仅使用合成内容。无法浏览器验证则如实记录，不把 Node VM 当真实浏览器。
- 源码、单元/接口测试、浏览器及原生/真机、安装发布分别报告。自动同步需要下方新增验收，不能仅凭原本机 CRUD 测试宣称通过；手机笔记和安装版更新仍为独立范围。

执行会话各自在完成本轮后释放自己的 claim、在自己的聊天交付结果，不轮询主会话，不发确认消息链。主会话在用户调用时复核集成，不依赖任务完成自动唤醒。

## 自动同步追加约定

本节是用户选择后的增量范围，由主会话确定信任和合并架构，优先于前面的本地版假设。复用本地版候选，不推倒重写，也不顺带修复无关安全问题。

### 用户流程与设备信任

1. 各电脑升级到支持笔记同步的版本，在笔记页一次性开启“自动同步”，选定自己的电脑。每台参与设备均须开启并相互选入；只有这些设备属于本轮同步范围。
2. 服务已配置的 network.auth_token 至少 32 字节，且参与电脑使用相同值；新界面不接收、返回或储存这个秘密，只复用既有服务认证。尚未具备认证条件时，本机笔记继续可用，同步显示“需要先配置设备认证”，启用请求返回明确错误，不能悄悄退为匿名同步。
3. 同步选择的是经 Tailscale 发现的稳定 node_id，最多 16 个；可选择已发现的离线设备。不能根据用户填写的 URL、笔记 origin.addr、请求中的自报设备 ID 或任意域名决定连接目标。
4. 每次网络读取，服务端用 ConnectInfo 的实际来源 IP 与当前 Tailscale 发现结果映射出唯一 peer，核对本机已启用、peer 被选入和有效认证；请求自报字段不提供权限。拒绝非 Tailscale 来源、未知/未选入设备、匿名请求和浏览器 Origin/Fetch-Site 访问。两端没有相互授权时明确显示等待授权，不自动扩大到全部 tailnet。
5. HTTP 仅走 Tailscale 发现的设备 IP 与配置服务端口；专用客户端禁用重定向，不能将认证头带到其他 URL。发现/连接/读取均有有限超时，禁用同步或移除设备后立即停止其新请求，并在读取响应/合并前重新核对设置；在途响应不得绕过撤销。已有本地副本保留，撤销不是远程擦除。

此处“私有同步”依赖 Tailscale 和相同服务令牌，不承诺抵御已授权同步组成员伪造内容，也不把普通同步授权升级为账号库权限。手机的现有配对服务不自动加入这个同步组。

### 离线版本与合并

同步在独立表中保存本地 replica UUID、设置和版本元数据，普通剪贴板表不承载同步数据。现有笔记在首次使用时事务性补充副本元数据，保留 id、正文、任务、时间和原本机 revision；不重建数据库。

- 每份完整笔记/清单版本带 `{replica_uuid: counter}` 的有序因果向量，以及确定性内容版本 ID。每个本地修改在同一事务内递增本副本计数、保存内容和同步元数据；记录级 revision 仍只是本机 CAS 版本，不能用它或墙上时钟给不同设备排序。
- 版本 ID 是固定字段顺序的 v1 typed serde 表示的 SHA-256，覆盖内容和向量，不覆盖本机 revision。接收端重算验证。明确这是本协议的摘要域，不声称实现跨语言通用 JSON canonicalization。
- 同一 note_id 下，因果上更旧的版本忽略，更新的版本取代其祖先，相互并发的版本全部保留。相同向量但内容版本不同也保留为副本分叉冲突，不能当作重复丢弃。重复交换幂等，不产生新 revision 或新的本地编辑。
- 可见主版本采用各设备相同的确定性规则：有未回收分支时优先这些分支，再按 version_id 升序选主版本；其余版本保留供用户选择。这个规则只用于显示，不意味着自动删除其他内容。
- 已存在多分支时，普通编辑/删除/恢复返回 409 error=sync_conflict。用户明确选中保留版本后，服务端合并全部已知向量取分量最大值，再递增本副本计数，形成一个因果后继来结束该冲突。未保存输入可先另存为新笔记，不能被这个流程覆盖。
- Todo 第一版按整份清单版本同步；不同设备并发勾选/编辑同一清单时保留冲突，不宣称逐任务 CRDT 自动融合。没有并发的离线勾选在重连后正常补齐。
- 回收站状态属于版本内容；回收和恢复都产生新版本并自动传播。旧的离线副本不能覆盖因果更后的回收状态；回收与编辑并发时保留两份，不能静默丢失编辑。第一版没有永久删除/墓碑回收，不按时间清掉删除信息。
- 因果向量最多 64 个合法 replica UUID；单个笔记最多 16 个并发头。超过限制、格式非法、类型变更、摘要不匹配或配额超限时整批拒绝且不确认成功，不丢弃已有分支来腾空间。有效内容/冲突头仍受 32 MiB 约束，含同步元数据及可见副本的实际持久化总量另设 64 MiB 上限。

该因果向量与冲突保留原则参考 [Dynamo 的版本分支及应用层协调](https://www.allthingsdistributed.com/2007/10/amazons_dynamo.html)，这里没有引入其完整分布式存储架构。

### 自动交换和状态接口

运行 daemon/tray 时每约 15 秒对已选在线设备作一次有界的后台拉取，离线使用有界退避，重连后继续；这不是实时延迟承诺。两台设备都定期拉取对方的已授权记录，实现双向传播，不需要对外开放笔记写入接口。后台任务仅从正式 serve 生命周期启动，router 构造/测试不启动，服务退出须取消任务，不能留下持有旧数据库的孤儿 worker。

本机 API 继续使用上文的严格 loopback/Host/Origin/site 门禁：

| 请求 | 追加契约 |
| --- | --- |
| GET /v1/notebook/sync/settings | `{enabled,peer_ids}`；初始 disabled，选定设备持久化 |
| POST /v1/notebook/sync/settings | `{enabled,peer_ids}`，拒绝未知字段、重复/未知 node_id；开启时校验认证条件；200 返回设置；关闭/移除立即影响后续读取和合并 |
| GET /v1/notebook/sync/status | `{enabled,ready,needs_authentication,peer_ids,conflicted_items,peers:[{node_id,host,state,last_success_at,error}]}`；state 为 disabled/offline/checking/checked/pending/unsupported/denied/error/quota_exceeded；error 只含通用错误码，不含内容/令牌 |
| POST /v1/notebook/sync/run | `{}`，请求一个有界立即检查，202；已有周期运行时合并触发，不并发重复同步 |
| GET /v1/notebook/{id}/conflicts | `{versions:[{version_id,device_id,kind,title,markdown,tasks,pinned,trashed,created_at,updated_at}]}`，只本机可见；device_id 不作为用户标签展示 |
| POST /v1/notebook/{id}/resolve | `{expected_revision,version_id}`；明确选择当前冲突版本，事务内生成协调后的后继并返回完整 NotebookItem；旧 revision 或不在当前分支中的 version_id 返回 409 |

NotebookItem 和 NotebookSummary 增加只读 `version_id` 和 `conflict_count`；写请求不能接受这些客户端自报字段。原 revision 冲突仍返回 revision_conflict，自动同步后的多分支返回 sync_conflict，前端分别处理。

独立网络同步端点只有 `GET /v1/notebook-sync/records?after=<note_uuid:version_sha256>&limit=16`，它不属于本机编辑白名单。必须通过本节设备/令牌门禁，不因普通 auth_middleware 允许无 token 而放行。响应为 `{protocol:1,items:[{note_id,versions:[SyncVersion]}],next_after:null或最后游标}`；每个 items 元素只携带一个版本，同一 note_id 的多个冲突头允许分布在不同页面，避免整份多分支文档大于响应限制。游标由合法 note UUID、冒号和 64 位小写十六进制 version_id 构成，不承载权限。内容包含回收状态/冲突头，不含本机 revision、密钥、任意路径或无关剪贴板。SyncVersion 包含 version_id、clock、author_replica_id 和 content（上述 NotebookItem 的内容/时间字段，不含 revision/version_id/conflict_count），kind/id 必须一致。

按 `(note_id,version_id)` 稳定顺序分页，每页最多 16 个版本且编码总响应不超过 1 MiB，至少能容纳一个合法版本；无法容纳的版本明确报错而非空页跳过。请求/响应均严格校验 protocol/ID/条目数/字节数，接收流读取时即限制 1 MiB，不先无界读到内存；分页游标必须前进，重复/倒退/无效游标终止该轮。每轮每个 peer 最多 32 页，未完成的游标下轮续读，完整遍历后归零重扫；失败不前移游标。分页期间修改的记录在后续遍历补齐，不凭一次分页宣称全局原子快照。

端点仅提供受认证的只读数据；每台 worker 将接收批次在自己数据库中原子合并。失败不推进到后续页，不把被拒绝的数据认作已持久化。源副本和本地编辑仍保留。服务间 404 表示旧版不支持，401/403 表示认证或双向选择未完成，均不得退回剪贴板通道或自动降级匿名。

`last_success_at` 只证明最近读取/合并检查成功，不能单凭 HTTP 200 宣称本机改动已被所有对端保存。界面显示“自动同步已开启 / 最近检查 / 等待设备 / 有冲突”等实际状态；未实现对端确认前不显示“全部设备已同步”。

### 界面补充与任务边界

A 增加中英文同步开关、已发现设备勾选、前置条件/离线/未授权/旧版/容量错误提示、检查按钮、冲突入口和明确选择版本。可见页面定期刷新本机状态与列表，不把远端刷新覆盖到 dirty/composing 的编辑缓冲；远端更新只提示并走 CAS/冲突流程。原“发送”改成可选的“作为文本发送”，与自动笔记同步区分。JS/CSS/HTML/前端测试路径沿用 A 原范围。

B 在原 notebook.rs/store.rs/lib.rs/server.rs/tests/notebook.rs 范围外，允许新增 `src/notebook_sync.rs` 与 `tests/notebook_sync.rs`；仍不改 Cargo、现有 PeerClient/协议/配置、Flutter 或 A 的文件。同步设置独立持久化，不扩散为全局配置重构。主会话已经确定上述架构和授权规则，B 按此实施；遇到必须扩大授权或修改其他文件的情形停在依赖部分报告。

### 新增验收

1. 两个隔离副本的创建、修改、Todo 勾选、回收、恢复能交换并收敛，关闭再打开数据库仍保留版本信息；重复交换不新增副本。
2. 离线多次修改后重连补齐；并发正文、清单、回收/编辑保留冲突；选版本协调后再交换收敛，迟到旧副本不复活已回收笔记。相同向量不同内容有测试。
3. 网络门禁覆盖未开启、无令牌/短令牌、错误令牌、未知来源、未选 peer、伪造身份/Origin、allow_remote_apply 绕过、移除设备与在途响应；本机编辑 API 仍不能被同步对端访问。
4. 有界分页、单条/总量/时钟/头数限制、错误 JSON、摘要不匹配、超大流、游标不前进、旧版设备、失败/重启重试以及 worker 退出清理有验证；任何失败不损坏本地数据。
5. 自动同步不开启剪贴板采集、不产生普通历史、不改真实剪贴板、不发布网络文件或账号库数据。
6. A 的合成测试覆盖状态变化、dirty/composing 下远端更新、sync_conflict、冲突版本选择、设备授权和禁用操作；保留原功能回归。分别报告内存/数据库副本测试、HTTP 夹具和真实 Tailscale 双机验收，不能相互替代。
