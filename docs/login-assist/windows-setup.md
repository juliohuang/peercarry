# Windows 本机安装与联调

本文用于 Windows 本机验证原型。测试和示例只使用合成账号、密码与验证码；不要粘贴真实凭据。Android APK 是调试构建，不属于正式发布包。

## 构建

已有本地预览包 `target/login-assist-windows.zip` 时，解压到固定目录，在 Chrome 加载其中的 `extension`，然后在解压目录运行 `node .\install-host.mjs <32位扩展ID> .\peercarry-login-host.exe`。该脚本需要 Node.js。不要移动已加载的扩展目录，移动后须重新检查扩展 ID 并重新安装 host。

从源码构建则按下面步骤操作。构建完成后可运行 `node apps/login-assist-browser/package-windows.mjs` 重新生成预览包。

在仓库根目录执行：

```powershell
cargo build --release -p peercarry-login-assist --bin peercarry-login-host
cd apps/login-assist-browser
npm ci
npm run build:extension
```

host 输出为 `target\release\peercarry-login-host.exe`；解压扩展目录为 `apps\login-assist-browser\extension`。用 Chrome 打开 `chrome://extensions`，启用开发者模式并选择“加载已解压的扩展程序”，选择该 `extension` 目录。复制 Chrome 显示的扩展 ID；不要将 ID、配对票据或票据内容写入日志/文档。

安装脚本必须使用实际扩展 ID 和构建出的 host 路径。在仓库根目录运行：

```powershell
node .\apps\login-assist-browser\install-host.mjs <32位扩展ID> .\target\release\peercarry-login-host.exe
```

脚本要求扩展 ID 为 32 个 `a` 到 `p` 字符。Windows 下它将 host 复制到 `%LOCALAPPDATA%\PeerCarry\LoginAssist`，写入 host 配置和 Native Messaging manifest，并注册当前用户的 `HKCU\Software\Google\Chrome\NativeMessagingHosts\dev.peercarry.login_assist`。**该操作不需要管理员权限，不改防火墙，不更改 80/443 服务。**扩展首次访问当前标签页仍需用户点击扩展 action。

## 本机配对验证

确认 Chrome 已加载扩展且安装的 manifest `allowed_origins` 与当前扩展 ID 一致。在扩展 popup 中勾选本机授权确认，使用 loopback 地址启动验证；启动后显示的二维码/票据是短时敏感材料，只供当前测试使用。Flutter 客户端既可手动输入票据，也提供 QR 扫描入口。Dart 集成测试会通过 stdin 临时传票据，不应将其输出或保存。

当前已验证真实 Chromium 的 Native Messaging、loopback TLS、证书 pinning、合成 DOM 定向填写、捕获裁剪、布局变化撤销和越界点击拒绝。Dart 客户端也已真实连接本机 TLS host。**这不证明手机摄像头扫码、手机设备或跨设备网络可用，也不代表任何真实网站登录。**真实手机/摄像头/跨设备网络以及 Mac/iPhone 验收由用户另行安排。

## 手机连接 Windows 的待验收步骤

1. Windows 与手机连接同一可信局域网或 Tailscale 网络；本次未修改防火墙规则，若网络不可达需单独确认 Windows 入站权限。
2. Windows Chrome 打开待协助的 HTTPS 登录页，点击扩展，填入本机明确的私网或 Tailscale 地址，例如 `100.x.x.x:5198`。手机不能使用 `127.0.0.1`；客户端会拒绝手机上的 loopback 票据。
3. 用户先停止其他自动化，再勾选确认并启动。手机进入“登录协助”扫描 60 秒内有效的二维码，检查实际站点 origin 后明确授权。
4. 选择字段并填写；截图、点击和扩大可见区域均需显式操作。页面布局或文档改变后先刷新绑定，再重新授权。后台切换/断线会中止协助，重新连接需新配对。
5. 站点登录完成后，用户明确确认交还。当前仅撤销手机通道并结束手动协助，不恢复 AI，也不自动证明网站登录成功。

以上步骤仍需真机验收，本机 E2E 不替代该验收。

## 停止与崩溃锁恢复

正常结束请在扩展 popup 中停止 host，并关闭相关测试。安装脚本发现 host lock 时会拒绝覆盖，以免干扰活动会话。

只有确认该 host 已崩溃且无人使用后，才可由用户显式运行本地恢复：

```powershell
& "$env:LOCALAPPDATA\PeerCarry\LoginAssist\peercarry-login-host.exe" --recover-lock
```

恢复命令尝试获取并清除 stale lock；活动进程仍持有锁时应拒绝，且不重用旧配对、旧授权或旧输入。成功后需重新显式启动 host 并发起新配对。不得通过删除锁文件、重装或启动第二个 host 绕过拒绝状态。

## Android 调试包

需要 Flutter SDK 和 Android 构建环境。在仓库根目录执行：

```powershell
cd apps/mobile
flutter build apk --debug
```

输出为 `apps\mobile\build\app\outputs\flutter-apk\app-debug.apk`。它是调试构建，不是签名的正式发布 APK；生成 APK 不等于真机扫码、网络或登录验收。
