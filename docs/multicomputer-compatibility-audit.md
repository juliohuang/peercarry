# 多系统兼容性检测（2026-09-26）

## 范围与结论

检测当前工作区的桌面程序，以及快捷面板、提醒切换、传输中心、离线发件箱相关路径。主要范围为 Windows、macOS、Linux；Android/iOS 仅核对独立 Flutter 客户端的声明和发行边界。

目前不能声明多系统完整兼容。Windows 本机构建及自动化测试通过；Linux core/CLI 在 Ubuntu WSL 的编译检查通过，选定核心回归 40 通过、2 失败（测试脚本只适配 Windows）；原生 Wayland 存在明确的实现缺口，X11 剪贴板对象生命周期也有风险；macOS 尚未在本轮完成原生构建或实机验收。发行 workflow 矩阵不等于本次 CI 已执行成功。

本轮没有修改产品源码、安装系统依赖、发布安装包或替换运行中的版本。三个 Luna 子代理分别检查原生壳、核心平台代码、打包及平台声明；主代理复核了实际实现和锁定版本依赖源码，执行 Windows 与 WSL 验证。

## 平台矩阵

| 平台 | 仓库声明或构建目标 | 本轮证据 | 尚未闭环 |
| --- | --- | --- | --- |
| Windows x64 | 安装器最低 Windows 10；`x86_64-pc-windows-msvc` | Rust 1.95.0；CLI/托盘开发构建通过；workspace 143 项测试通过，0 失败/忽略 | 原生热键、WebView2、DPI、实际剪贴板、安装升级及双机传输未实测；存在 1 处原有 `unused_mut` 警告 |
| Linux x64 / core、CLI | `x86_64-unknown-linux-gnu`，glibc 平台 | Ubuntu WSL2、Rust 1.98.0；core/CLI 编译检查通过；选定回归 40 通过、2 失败 | 两个传输集成测试使用 Windows `.cmd` 模拟程序，需跨平台适配；WSL 验证不等价于发行版桌面验收 |
| Linux X11 / 托盘 | GTK 3、libxdo、WebKitGTK 4.1；文件引用需 xclip | 平台实现与依赖静态检查 | 离线编译缺少缓存 `xkeysym 0.2.1` 而停止；WSL 当前也缺 pkg-config，未证明 GTK/WebKit 开发库齐备；托盘、热键与文件选择器未实测 |
| Linux 原生 Wayland | 文件 URI 有 wl-copy/wl-paste 分支 | 确认主/快捷窗口的 child WebView 接口仅接受 X11；文本图像 backend 与全局快捷键也有 X11 限制 | 不具备完整的原生 Wayland 支持，不能用文件 URI 分支代表整个产品支持 Wayland |
| macOS Apple Silicon / Intel | arm64、x86_64 两套目标；DMG 声明 macOS 11 起 | 平台分支、窗口/文件剪贴板及打包源码检查 | 本轮无 macOS 原生编译与 WKWebView 实测；Developer ID 签名/公证未配置；`.app` 应用内替换升级明确未支持 |
| Windows ARM64 / Linux ARM64 | 当前桌面发行矩阵未列出 | workflow 静态证据 | 未验证原生构建或模拟运行，不作为已支持的原生发行目标 |
| Android / iOS | 独立 Flutter 客户端；Android SDK 值继承 Flutter；iOS 工程最低 13 | 工程/文档静态清单 | 不自动获得桌面四功能及 HTML 改版；本轮未做 Flutter 构建或真机验收，当前桌面 release workflow 不发布移动包 |

## 已确认的问题与限制

### 0. 传输回归测试仅适配 Windows（本轮 Linux 实际失败）

`crates/peercarry-core/tests/transfer_jobs.rs:241-246` 与 `:365-370` 无条件写入 `fake-tailscale.cmd`，内容为 `@echo off`、`type`，随后将该路径交给进程执行，没有 Unix 脚本分支。

本轮 WSL 运行中：

- `download_retry_reuses_completed_file_without_writing_clipboard` 返回 `Exec format error (os error 8)`，随后无法到达预期的第二文件失败注入点。
- `queued_send_waits_for_exact_offline_peer_then_pushes_saved_entry_once` 没有成功创建发送任务，在读取响应 `id` 时触发 `Option::unwrap()`。

这说明当前测试夹具不兼容 Linux，并使跨系统回归门禁失败；不能直接推导为 Linux 传输产品实现故障。macOS 执行同一无条件 `.cmd` 夹具也存在相同结构性问题，但本轮未在 macOS 运行确认。应提供 Windows/Unix 各自可执行的模拟程序，或改用跨平台测试辅助二进制，然后重跑对应平台回归。

### 1. 原生 Wayland 主窗口和快捷面板创建路径不兼容（优先）

- 项目 `crates/peercarry-tray/src/main.rs:261`、`quick_panel.rs:33` 均使用 `build_as_child(&window)`。
- 锁定的 Wry 0.46.3 `src/webkitgtk/mod.rs:97-113` 在 child 创建时只匹配 `RawWindowHandle::Xlib`，其他类型直接返回 `UnsupportedWindowHandle`。
- Tao 0.32.8 的 Linux 实现使用 GTK（`platform_impl/linux/event_loop.rs:217-219`），并能返回 Wayland window handle（`window.rs:946-953`）。所以当实际 GTK 会话使用 Wayland handle 时，该组合无法建立嵌入式 WebView。
- 主窗口失败时会尝试外部浏览器（`main.rs:1355`）；快捷面板失败仅记录日志（`main.rs:208`）。浏览器回退不等价于原生窗口、文件选择、拖放和快捷面板均可用。
- 全局快捷键另依赖 `global-hotkey 0.8.0`，其 README 与 Linux 实现明确仅支持 X11。

建议把 Linux 嵌入式窗口改为 Wry 文档支持的 GTK 容器路径，再单独明确 Wayland 热键支持或降级行为。本轮仅确认源码支持边界，未启动实际 Wayland 会话。

### 2. Linux 文本/图片剪贴板写入的对象生命周期过短（优先）

`crates/peercarry-core/src/clipboard.rs:114-131` 每次创建局部 `arboard::Clipboard`，执行 set 后函数返回即释放。

arboard 3.6.1 的 X11 backend 虽有全局单例和服务线程，但最后一个 Clipboard handle drop 时会请求 clipboard manager 接管，然后清除全局状态、销毁 X window 并结束线程（`src/platform/linux/x11.rs:1082-1105`）。因此，若没有管理器接管或接管不及时，界面可能已报告复制成功，但其他应用无法继续粘贴内容。

这是源码确认的生命周期风险，未在真实桌面剪贴板动态复现；不能概括成每次必然失败。建议以长生命周期对象持有 selection，或实现明确的等待/交接机制。

### 3. 纯 Wayland 文本/图片 backend 未启用

工作区 `Cargo.toml:42` 仅声明 `arboard = "3"`。锁定的 arboard 3.6.1 默认 feature 为 `image-data`，`wayland-data-control` 是单独的可选 feature，当前没有启用。

因此现有文本/图片读写使用 X11/XWayland backend；没有 XWayland 时不能视为支持纯 Wayland。Linux 文件引用模块单独调用 wl-copy/wl-paste，不能补齐文本与图片 backend。

### 4. 下载目录必须支持硬链接

普通文件下载在 `crates/peercarry-core/src/client.rs:545` 使用 `hard_link` 将完整临时文件发布到最终文件名；失败会直接返回 `hard link required`，没有其他发布路径。

因此不支持硬链接的目标文件系统（例如 FAT/exFAT 或部分网络挂载）不能完成此流程，即使下载字节已接收完整。该限制跨 Windows/macOS/Linux；目录归档使用另一条 rename 路径。改进时应继续保持无覆盖、完整文件才发布的约束。

### 5. 跨系统文件名与 Linux URI 仍有边界

- 目录 tar 内部条目在 `client.rs:675` 一带检查路径穿越与文件类型，但没有目标系统文件名兼容策略。Unix 合法而 Windows 不接受的名称，例如包含 `?`、保留设备名或尾随点/空格，可能导致解包失败或不能原样落地。本轮未执行这些跨机文件名样本。
- `clipboard/linux.rs:109` 将 `file://` 前缀直接移除，`file://localhost/tmp/a` 会变成相对路径 `localhost/tmp/a`，未区分 URI authority 与本地绝对路径。当前 URI 单元测试只覆盖空格和 Unicode 的往返编码。

### 6. Linux 文件选择器依赖 Portal 后端

当前 rfd 0.15.4 默认启用 XDG Desktop Portal，项目没有启用 GTK3 备选 feature。需要可提供文件选择 API 的桌面 Portal 后端；只有不实现该 API 的后端时，文件/文件夹选择不能当作可用。WSL 编译检查不能证明实际桌面 Portal、托盘图标或文件拖放行为。

## 其他一致性与验收缺口

- AI 提醒选择/设备 ID 等使用 `paths::data_dir()`，传输队列还会遵循 `storage.data_dir`。自定义存储目录时语义不一致，是共用配置问题，不属于特定系统故障。
- Windows WebView2、macOS WKWebView、Linux WebKitGTK 是不同渲染器；此前 Chromium 截图不能证明另两个引擎的布局、键盘、焦点与缩放均通过。
- README 的历史平台“已验证/未验证”描述不作为当前工作区的验收结果。
- 未把 Ubuntu 22.04 的 WebKitGTK 包可用性列为故障：本轮没有目标 apt 索引或 CI 失败证据。

## 验证记录

- Windows：`cargo test --workspace --locked --offline -- --test-threads=1`，143 通过、0 失败、0 忽略；完整 UTF-8 日志位于 `target/compatibility-audit/windows-tests-utf8.log`。
- Windows：`cargo build -p peercarry-cli -p peercarry-tray --locked --offline`，通过；`target/compatibility-audit/windows-build.log`。
- Linux：复用现有 Cargo 缓存的 `cargo check -p peercarry-cli --locked --offline`，通过；`target/compatibility-audit/linux-check-shared-cache.log`。初次使用 WSL 自身空缓存时缺 arboard，复用已缓存依赖后已解决该环境阻塞。
- Linux 托盘：`cargo check -p peercarry-tray --locked --offline`，未完成，缺离线缓存 xkeysym 0.2.1；`target/compatibility-audit/linux-tray-check.log`。该结果是环境阻塞，不作为源码编译错误。
- Linux 核心回归：`cargo test -p peercarry-core --lib --test desktop_tools --test ai_receiver_switch --test transfer_jobs --locked --offline -- --test-threads=1`，40 通过、2 失败。库单测 35/35、提醒认证 1/1、本地桌面端点门禁 1/1、传输集成 3/5；失败根因见问题 0。日志为 `target/compatibility-audit/linux-tests-utf8.log`。构建输出和临时测试数据均放在项目 `target/compatibility-audit` 中，未写入默认运行数据目录。

原生验收仍需要 Windows/macOS/Linux X11/Wayland 各自验证：冷启动、热键/托盘唤起、中文输入与缩放、文本/图片/文件复制与实际粘贴、离线重开与恢复、两机传输、下载目录文件系统及不同文件名，以及安装/升级流程。
