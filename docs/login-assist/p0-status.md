# P0 实现与验证记录

日期：2026-09-19。**状态：Windows 本机技术链路已通过真实 Chromium E2E；真实手机摄像头/跨设备网络及 AI 执行器交还尚未验收。本功能仍是验证原型，不可用于真实凭据。**

## 已实现

- `crates/peercarry-login-assist`：独立 Rust 状态策略与本机 Native Messaging host。专用 TLS listener 仅在用户确认后启动；单次配对凭据换取会话 token，客户端 pin 主机证书；配对票据有效期 60 秒，失联会撤销授权。此链路不接入剪贴板存储、现有 HTTP API 或 MCP。
- `apps/login-assist-browser`：Chrome MV3 扩展通过 Native Messaging 绑定实际 tab/document/frame/origin/字段，并执行一次性定向填充、真实浏览器画面采集和电脑端裁剪；扩展 manifest 使用 `activeTab`，没有 `host_permissions`。页面/布局变化会撤销旧 epoch，查看区域与点击区域分开授权。
- `apps/mobile`：Flutter 登录协助界面、票据文本输入/QR 扫描入口及 Dart pinned TLS 客户端已实现。Dart 客户端通过真实 TLS 服务的集成链路验证；这不代表手机摄像头扫码或真实手机网络验收。
- `peercarry-automation`：协作暂停 barrier 本地单元测试通过，可等待本进程内进行中的后端调用结束。它尚未通过 Native Messaging 与登录 host 建立跨进程握手，也没有接入外部 AI 执行环境。
- 崩溃锁默认保持阻塞。仅用户显式运行 host 的 `--recover-lock` 才尝试本地恢复；若 host 正活动并持有锁，恢复命令应拒绝。重新开始需要显式发起新配对，不恢复旧授权。

## 已验证与未验证

| 范围 | 结果 | 边界 |
|---|---|---|
| Windows Chromium + 扩展 + Native Messaging host + pinned TLS | 真实浏览器 E2E 通过 | 仅合成账号、密码、OTP；自动化测试副本为 headless 授权 `activeTab` 困难，临时加了 `<all_urls>` 测试权限。发布扩展 manifest 不含 `host_permissions`；测试副本不代表发布权限配置。 |
| 浏览器 DOM 与画面 | 真实 DOM 填充、事件、重放拒绝、布局变化撤销、越界点击拒绝、真实截图裁剪与撤销流程通过 | 使用本机合成登录页与合成值；不是实际网站登录验收。 |
| Flutter/Dart 客户端 | Dart 客户端真实连接本机 TLS host 的自动化集成通过 | 不是手机设备、摄像头或跨设备网络验收。 |
| Rust 状态机与裁剪 | 当前本地策略测试通过 | 仅证明被测策略/合成像素路径。 |
| AI Runner | 同进程 cooperative barrier 本地测试通过 | `manual` 模式在交还后返回 `ai_resumed=false`；未接通 host 跨进程确认或外部 AI，不能宣称 AI 自动恢复已完成。 |
| 真机与环境 | 待用户稍后安排 | Mac/iPhone、真实手机扫描与摄像头、跨设备网络均未验收。Windows release exe 和 Android debug APK 已重新构建；APK 不是正式发布包。 |

Windows E2E 的临时测试 manifest 会在 `target/login-assist-e2e` 下创建，并在退出时清理注册项和临时 profile。扩展 ID、配对票据和凭据均不得写入交付记录。详细步骤见 [`apps/login-assist-browser/TESTING.md`](../../apps/login-assist-browser/TESTING.md)；Windows 安装说明见 [`windows-setup.md`](windows-setup.md)。

## 本地复核命令

本轮本地结果：`cargo test --workspace --locked` 通过，登录协助 23 个策略测试及 2 个 TLS/framing 测试通过；Node 13 项通过；Flutter 11 项通过，1 项既有真实剪贴板后端测试因未配置隔离连接而跳过；Flutter analyze 无问题。活动锁拒绝恢复、并发 host 拒绝、正常清锁、测试进程崩溃恢复和重启均通过。Clippy 成功，保留原策略函数参数过多等既有警告；不作零警告声明。

```powershell
cargo test -p peercarry-login-assist
cargo test -p peercarry-automation handoff
cd apps/login-assist-browser
npm test
npm run build:extension
```

这些本地检查不能替代下表中的设备/执行环境验收，也不构成产品发布批准。

## 原计划中的后续验收

原实施方案中的 18 项产品验收目标保留，当前没有整体通过：同一真实会话完成与外部执行器恢复确认、跨设备配对与断线、真实手机扫码和摄像头、真实手机私密输入、权限拒绝/重启恢复、各平台行为，以及实际网站登录仍需逐项给出证据。计划的首个环境 Mac/Chrome/iPhone 尚未验收；Windows/Android 当前为新增平台的本机原型，不替代原目标。

不记录真实凭据、配对 secret、token、扩展 ID、票据内容或测试数据库。安装只写当前用户 Native Messaging 注册项；不会修改防火墙，也不更改现有 80/443 服务。
