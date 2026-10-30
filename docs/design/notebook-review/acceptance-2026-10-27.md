# PeerCarry 笔记与待办界面验收记录

日期：2026-10-27

范围：集成后的笔记、待办、语言选择和响应式界面；验收数据限定为隔离探针生成的合成数据。

## 结果

| 检查项 | 结果 | 证据 |
| --- | --- | --- |
| 笔记、工作台和语言自动化回归 | 通过，35/35 | `node --test scripts/tests/notebook.test.cjs scripts/tests/workbench.test.cjs scripts/tests/ui-locale.test.cjs` |
| 四项草稿保护 | 自动化回归通过 | 测试覆盖跨笔记置顶前保存、IME 组合期间不保存、迟到 GET 不覆盖输入、迟到 POST 不夺走当前编辑器 |
| 隔离探针启动 | 通过 | 探针绑定 `127.0.0.1` 随机端口并输出 `http://127.0.0.1:64061`；数据目录为 `target/notebook-integration/acceptance-20261027-b/isolated-data` |
| 独立 HTTP 客户端访问 | 未通过，环境阻断 | 对 `/`、`/assets/notebook.js`、`/assets/notebook.css`、`/assets/ui-locale.js`、`/v1/notebook/sync/status` 的请求均无法连接 |
| Edge 和应用内浏览器实际渲染 | 未完成 | Edge 与应用内浏览器打开同一地址均返回 `net::ERR_CONNECTION_TIMED_OUT` |
| 中英文实际页面验收 | 未完成 | 页面无法加载，不能确认最终浏览器渲染和系统语言来源 |
| 940×680、560×420 布局验收 | 未完成 | 页面无法加载；没有生成或伪造截图 |
| macOS、Linux 原生验收 | 未运行 | 本次只有 Windows 主机；不据此宣称跨系统通过 |

## 语言与平台边界

笔记渲染器的自动化测试分别验证了英文和中文标签；`ui-locale.js` 优先使用宿主注入的 `__PEERCARRY_LANG__`，未注入时读取浏览器首选语言并回退到英文。桌面托盘的系统语言检测在 Rust 中按 Windows、macOS、Linux 分支实现。此次没有进入实际 WebView，因此“电脑系统语言最终传到笔记页”的链路仍需在真实桌面端核验；独立浏览器则按其首选语言选择界面语言。

## 浏览器阻断记录

在 Codex 管理的命令会话中以前台方式运行隔离探针，服务成功输出随机回环地址。随后从独立 PowerShell HTTP 客户端请求页面、脚本、样式和状态接口，均返回“无法连接到远程服务器”。再分别通过 Edge 扩展和应用内浏览器访问该地址，两者均超时。探针由本任务创建，并已通过 Ctrl+C 停止。

现有证据表明命令会话与桌面浏览器无法共享该回环监听；这更像执行环境的网络隔离，尚不能据此判断页面或接口返回错误。没有申请扩大网络权限，也没有触碰真实剪贴板、用户数据库或 Tailscale 网络。

要在这台 Windows 主机上完成真实页面验收，请从**能被 Edge 访问的普通 Windows PowerShell**、在仓库根目录运行：

```powershell
cargo run -p peercarry-core --example notebook_review_probe -- target/notebook-integration/acceptance-20261027-c/isolated-data
```

命令输出随机 `http://127.0.0.1:<端口>` 后，在同一台主机的 Edge 打开该地址。完成验收后用 Ctrl+C 停止服务。届时应在 940×680 和 560×420 下分别检查中文与英文，并保存真实浏览器截图；本记录不把当前自动化测试当作这些验收的替代。

## 安全与工作区说明

探针使用独立数据目录、随机回环端口、合成离线设备和合成数据。本次没有改动集成代码、Rust 后端或现有测试；本文件是本次新增的验收记录。既有工作区改动均保留。
