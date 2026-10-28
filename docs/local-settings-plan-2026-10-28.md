# 本机设置流程：主会话架构约定

日期：2026-10-28。目标是修复已配置访问令牌后桌面设置页 401 的断点，同时避免把既有秘密读回网页。Terra A/B 已分别实现前后端；主会话已集成，并继续修复真实进程重启交接和浏览器恢复流程。下文保留实施约定，文末记录各轮验收范围；真实安装版、原生窗口及跨设备验收仍未完成。

## 决策与边界

- 新增独立本机设置接口；`/v1/config` 和 `/v1/actions/restart` 的既有客户端、数据和授权行为保持不变，不加入其免令牌白名单。
- 新接口代表已经控制本机 OS 用户环境的桌面用户。每次请求同时要求真实 ConnectInfo 为 loopback、Host 为 localhost/回环 IP；Origin 若存在必须等于当前 `http://Host`；Sec-Fetch-Site 若存在只接受 same-origin/none。缺失 ConnectInfo、伪造 Host/Origin、浏览器跨站、网络来源均拒绝。即使提供有效 peer token 或 `allow_remote_apply=true` 也不能绕过。
- 只将下文两个精确路径加到桌面同源白名单；各新路由自身仍使用上述严格门禁。方法由 GET/POST 路由及 handler 明确限定，不通过宽泛路径前缀放行其他 API。
- 页面只收到允许编辑的非秘密字段和秘密“已配置”布尔值。网络令牌、AI 令牌、移动设备令牌都不读取返回、不写进 HTML/JS 初始化/localStorage/日志/错误。用户主动输入的新令牌只为本次明确保存提交；不能自动修改实际用户认证或同步设备。
- 不新增配对、自动密钥分发、全 Tailnet 授权、云端/账号库权限、后台剪贴板行为或真实部署。本轮只读实际源码，测试和保存均使用 target 下合成配置。

## 固定接口

### GET /v1/desktop/settings

无需向网页提供 peer token，但必须通过严格门禁。返回 `Cache-Control: no-store`，结构为：

```json
{
  "schema": 1,
  "revision": "opaque-revision",
  "instance_id": "public-random-process-id",
  "desktop_url": "http://127.0.0.1:5199/",
  "restart_recommended": false,
  "values": {
    "node.name": "Example",
    "network.port": 5199,
    "storage.download_dir": null,
    "apps": []
  },
  "secrets": {
    "network.auth_token": true,
    "ai_token": false
  }
}
```

`values` 是扁平键名映射：只包含当前 timeline 的 FIELDS 中所有非 password 字段，加 `apps`。完整集合由 B 在服务端静态列出，与 A 当前 FIELDS 对齐；不能直接序列化整个 Config。空 optional 文本统一返回 null，前端显示为空。`apps` 仍为 `{name,path,args}` 数组，属于本机管理内容，不发送给 peer。更新签名公钥是公开配置，照常返回。

必须读磁盘上的最新配置；文件不存在时使用启动配置作为未保存基线，GET 不创建文件。配置文件非法或读失败则返回通用失败码，不能回退成默认值后覆盖。隐藏的 mobile 配置、移动令牌及 transfer_idle/prepare_timeout 等不能出现在 values，也不能因界面不包含它们而被删除。

revision 用于防止旧页面覆盖：对当前文件原始字节（缺文件时为稳定的启动基线表示）计算进程内带私有随机盐的 SHA-256 摘要。随机盐使用现有 rand/uuid，不持久化、不返回。不能将完整配置的无盐摘要作为秘密口令的离线验证器；revision 不是访问凭据。服务重启后旧 revision 失效，页面需重新加载。

`instance_id` 是与 revision 私有盐独立的进程随机 UUID，不是访问凭据；同一进程普通保存保持不变，重启后变化。`desktop_url` 按保存配置给出回环首页候选，考虑完整 SocketAddr 自带端口与 IPv6；不保证已成功监听。GET/POST 都返回相对当前启动配置的 `restart_recommended`。旧前端可忽略新增字段；缺少这些字段的旧服务仍只显示未确认，不伪报恢复。

### POST /v1/desktop/settings

```json
{
  "revision": "revision-from-last-read",
  "changes": {"node.name": "My computer"},
  "secret_changes": {
    "network.auth_token": {"op": "replace", "value": "synthetic-example-token-at-least-32-bytes"},
    "ai_token": {"op": "clear"}
  }
}
```

- 请求仅允许 `revision/changes/secret_changes`；拒绝未知字段、未知配置键、类型不符、任意点路径、非法 secret 操作，以及在 changes 中传秘密。secret_changes 可省略；省略某项表示原值保持；replace 要求显式非空新值，clear 明确清除。不能把密码输入框留空解释成删除既有令牌。
- 对替换的两类令牌采用 32..256 个可见非空白 ASCII 字符；旧配置保留操作不借机改写既有秘密。新输入验证错误只返回字段/错误码，不回显输入。
- body 上限 256 KiB；只更新静态允许字段。请求 changes 可为空，不应为了普通保存发送未编辑字段。
- 新桌面接口的读改写在进程内串行；持锁重读并比对 revision，过期返回 409 `settings_conflict`，不修改文件或运行状态。解析/验证整批失败同样不写入。响应 200 使用与 GET 相同的脱敏 envelope，额外包含 `restart_recommended` 布尔值，供下一次修改/重启使用新 revision。
- 以磁盘当前 TOML 为基础做允许字段的补丁，保留未知/隐藏配置值和所有未改动秘密；复用 Config 验证。可使用现有 toml::Value，不新增解析依赖。原始注释/格式可能因 toml 序列化变化，文档和结果中说明；不宣称保留注释。缺文件时基于启动配置建立首次保存文件。
- 使用同目录唯一临时文件，写完并 sync 后原子 rename 替换；保留原文件权限，新文件在 Unix 限定 0600。失败不先删除/截断原文件、不改变内存下载目录；清理仅限自己创建的临时文件。不可把 Config::save 的普通覆盖误报原子写入。仅本接口参与串行锁，不宣称它与外部编辑器/旧 API 形成跨进程事务锁；提交前再核对基线。
- 仅成功提交后复用现有“下载目录即时生效”的行为；其他配置仍由既有重启流程生效。本轮不重构所有配置消费者。

### POST /v1/desktop/settings/restart

body 为 `{ "revision": "current-revision" }`，严格门禁、未知字段拒绝、有界 body。比对当前已保存配置 revision 后才复用既有重启实现；过期配置不重启。用户只在明确点击“保存并重启”后调用。测试必须使用 spy/stub 验证授权和调用，不真正 spawn/exit 或重启用户服务。

400/403/409/413/500 分别表示非法输入、来源门禁、配置冲突、过大、通用存储/配置失败；结构化错误形如 `{ "error": "settings_conflict", "message": "..." }`。不回显秘密、完整 TOML、配置原文或含秘密的解析器错误；不记录请求 body。新路由的读取、保存和错误均 no-store。

## A：桌面设置前端

原生会话：PeerCarry A｜笔记本界面与设备剪贴板；模型 Terra。

允许修改：`crates/peercarry-core/timeline.html`、`scripts/tests/ui-locale.test.cjs`、必要的 `scripts/tests/workbench.test.cjs`；可新增 `scripts/tests/desktop-settings.test.cjs`。不改 notebook.js/css、quick、偏好模块或 Rust。

- 设置加载、保存与明确重启改用以上独立接口。只保存本次更改的非秘密字段，apps 实际编辑后才作为 changes.apps 提交；采用 envelope revision。禁止退回旧接口读取秘密作为 401/404 降级。
- 非秘密 FIELDS 布局继续可用，秘密字段默认空白，旁边显示已配置/未配置和“保持/替换/清除”的明确操作。新值只能用于替换，不持久化，不参与普通设置快照或日志。保存成功后清空新值并更新状态；失败时保留用户正在编辑的内容并解释错误。
- 409 保留表单并提示配置已改变，提供明确重新加载而非自动覆盖；网络失败/接口缺失禁用无效提交并给可理解提示。请求期间防重复；不能把旧 GET/POST 响应应用到后来打开的不同设置上下文。
- 保留从笔记跳入时的 whenIdle/IME/草稿保护、认证字段定位、返回原笔记/待办。保存并重启只在保存成功后执行，重启尚未确认时不显示“已生效”。端口/绑定变更等恢复连接限制如实提示，不猜测重启成功。
- Node 合成回归覆盖新契约、秘密保持/替换/清除、失败/409不丢表单、首次配置和已配置场景、隐藏字段不提交、保存失败不重启、双语及返回草稿。真实 Edge 用合成 32 字符令牌验证已配置情况下加载和普通保存；不执行真实重启，不将 mock 当作服务验收。

## B：严格本机设置后端

原生会话：PeerCarry B｜笔记本数据与本机接口；模型 Terra。

允许修改：新增 `crates/peercarry-core/src/desktop_settings.rs`、新增 `crates/peercarry-core/tests/desktop_settings.rs`、窄改 `src/server.rs` 和 `src/lib.rs` 挂载/精确白名单/纯逻辑可见性。不改 Config 的既有 load/save、旧 API 行为、Cargo/锁文件、其他业务模块或 A 的文件。

- 按上述契约实现，主会话已确定信任、秘密和持久化边界；不能自行改成开放原 config/restart、回传令牌或加入新凭据系统。若原纯函数需要暴露，最多改 pub(crate) 并检查错误不回显秘密，不做无关重构。
- 测试使用独立 target 数据与合成 Config，覆盖已配置 token 下三路由可达、网络/缺 ConnectInfo/恶意 Host/Origin/Fetch-Site/有效 token+allow_remote_apply 绕过均拒绝；旧接口仍保持原授权。
- 验证 GET 不含三类秘密和隐藏配置，GET 不创建缺失文件，连续保存读最新值；部分补丁保留 mobile、隐藏 timeout 和未知值；过期/非法/配额/模拟写失败不改原文件，权限与原子替换；secret 保持/替换/清除、错误不回显、重启 gate+spy 不运行进程。不能只验证纯函数就宣称 HTTP 门禁通过。

## 协作与交付

- 同一 primary checkout/main；root 当前只认领文档，A/B 文件范围互斥。开始前读 marker/list，自己用官方 helper 创建 claim；普通沙箱写协作板被拒时可对自己的有界 claim/release 命令使用 require_escalated 提交自动审批，不能改 ACL/管理员权限/锁文件或其他会话认领。
- 基线在 `target/local-settings-20261028/before/` 与 `before-sha256.json`。不覆盖无关 dirty 文件，不切分支/worktree，不执行 Git 清理/恢复/合并，不提交/推送/安装/发布/部署，不引入依赖或全仓格式化。
- A/B 独立测试输出放 `target/local-settings-20261028/a/`、`b/`；不读取真实 config.toml、凭据、数据库、OS 剪贴板，不操作真实 peer。不得自动将发现设备加入笔记同步。
- 不递归委派。权限审批拒绝、同 hunk 冲突、无法满足契约或超界时停对应部分并报告，继续独立工作。不轮询对方；缺接口只标记依赖，不能用永久 mock 绕过产品构建。
- 完成后释放自己的 claim，在本会话交付文件差异、测试/构建结果和实际限制。主会话按用户调用时复核并统一集成；无自动唤醒或后台协调。真实服务重启、原生多系统及 Tailscale 双机验收单独报告。

## 主会话最终复核与验证

两名 Terra 完成只读交叉复核，主会话检查实际源文件、前后端契约、路由层顺序和存储路径。旧 config/restart 接口未增加免令牌路径；新接口在 peer auth 前后均验证严格的本机同源来源，错误和返回不包含三类既有秘密。

主会话补充并修正了三处问题：

1. **设置草稿离开保护**：普通字段、应用和秘密操作未保存时，标签/返回操作保留当前表单，提供继续编辑或明确放弃后离开的入口。保存进行中不能离开或重新加载，避免丢失结果；明确放弃时清除输入的新秘密。
2. **失败重试可见性**：首次 GET 失败前即初始化双语重载按钮文字，失败页也可辨认重试操作。
3. **累计待重启状态**：保存响应相对当前进程启动配置判断待重启项，忽略已即时应用的下载目录和动态读取的更新设置。后续下载目录/空补丁不会清掉此前的待重启修改；恢复启动值后取消提示。

三项前端回归与一项连续保存回归均保留修复前失败证据。最终验证：全工作区 Rust 178 通过、0 失败、0 忽略；七组 Node 回归 93 通过；格式及差异空白检查通过。Windows CLI/托盘与隔离探针开发构建、全工作区 Clippy 均通过，有告警。日志见 `target/local-settings-20261028/integration/`。

固定源码哈希重建后，真实 Edge 中英文 × 940×680/560×420 共 4 组全部通过。每组使用合成已配置令牌，验证脱敏加载、连续两次普通保存、普通/秘密草稿离开拦截、保持秘密不提交、409 保稿和明确重载、返回原笔记。与旧中间构建不同，最终组合未再出现第二次普通保存 500。没有发起重启请求；两个秘密字段的实际替换/清除、门禁绕过、隐藏值保留和原子保存由合成 HTTP/单元回归覆盖。

主要证据：`frontend-final.log`、`workspace.log`、`ui-regressions-before.log`、`restart-advice-before.log`、`restart-advice-final.log`、`probe-source-before.json` / `probe-source-after.json`、`edge-final/result.json` 及截图。隔离探针已停止，没有修改用户真实配置、令牌、数据库、剪贴板或设备授权。

该轮未执行重启；后续隔离进程验收见下节。安装版与原生界面重连仍未验收。Windows 文件安全复制经过本机测试，Linux/macOS 原生分支及 POSIX ACL 未完成当前整体验收。无法保留安全元数据/EFS 状态、符号链接配置等情况返回通用失败并保留原文件。进程内锁和提交前校验不能消除外部编辑器在最后校验与 rename 之间的竞争；TOML 序列化可能重排格式并移除注释。没有提交、推送、部署、安装或发布。

## 后续重启交接复核

主会话随后复现了启动参数丢失：生产重启处理器返回成功后，要求 `serve` 参数的隔离子进程因缺参退出。现已原样保留启动参数，并向子进程传递旧进程 PID 与启动时间；CLI/托盘在打开数据库前等待该身份退出，超时或身份不完整则失败退出。新旧重启接口共享防重入状态，重复请求返回 409 `restart_pending`，未放宽来源与权限校验。

Terra 只读复核后，真实隔离生产服务完成三种绑定/端口场景、共 6 次连续重启，新配置生效且笔记与待办完整保留。另两项独立进程验收确认旧进程存活超时和缺少交接参数时不打开 Store。全工作区 Rust 回归更新为 180 通过、0 失败、0 忽略。

详见[重启交接验收记录](restart-acceptance-2026-10-28.md)。记录明确区分生产服务探针入口、开发构建和用户安装版；不据此宣称真实设备或原生界面重连已验收。

## 后续浏览器恢复验证

同地址重启后，页面只探测当前同源的脱敏设置路由；不同进程的有效响应到达后重新加载表单和 revision，仍待重启的配置会继续提示。探测有单次超时、次数和总时间限制；离开/reload/pagehide 取消并检查迟到响应。新端口仅提供手动打开的回环首页入口，不增加跨源探测或 CORS。

为支持新地址入口，静态 `GET /` 新增严格的用户激活顶层导航路径：真实 loopback、单一本机 Host、无 Origin、浏览器 navigate/document/?1。该路径不能用于 API 或 iframe，设置/config/笔记的真实 HTTP 拒绝回归仍需通过。Terra 已完成只读边界复核。

真实 Edge 五组已通过，覆盖中英文、大小窗口、localhost 别名、network.port 和完整 network.bind 端口变化；每组重启后笔记、待办保留，后续普通保存成功。前端八项新增回归修复前失败，七组最终 101 项通过；最新全工作区 Rust 182 通过、0 失败、0 忽略。证据在 `target/reconnect-acceptance-20261028/`，完整边界见[重启验收记录](restart-acceptance-2026-10-28.md)。

源码提交另按桌面及公共依赖整理，保留基线 wire 协议与 CLI/路径兼容，未纳入 Flutter、登录辅助、账号库等独立改动。精确提交快照独立验证 Rust 149 项、前端 101 项通过；与上面的完整工作区历史验收分开计数。范围和证据见[源码提交说明](notebook-integration.md)。
