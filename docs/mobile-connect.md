# 手机免 Tailscale 连接

当前实现：局域网扫码直连，配置自建 FRP TCP 中继后跨网回退。手机只需要 PeerCarry。
桌面与手机之间使用 TLS，手机只信任二维码携带的电脑证书；中继转发 TLS 字节流，不持有电脑私钥。

## 使用

1. 运行包含本次改动的桌面托盘或 `peercarry serve`，在本机打开时间线（默认 `http://127.0.0.1:5199`）。
2. 进入“连接手机”，点击“生成配对二维码”。这一步才启用独立的 TLS 服务，默认端口 **5197**。
3. 手机进入“设备 → 扫码连接电脑”，核对电脑名称后确认。没有相机时，可使用“复制配对内容 / 手动输入配对码”。
4. 同一局域网优先直连；跨网时自动尝试二维码里的中继地址。手机显示“局域网直连 · 已加密”或“加密中继 · 端到端加密”。
5. 电脑可逐台“撤销连接”，或“停止手机连接”撤销全部会话并关闭 5197。

局域网需要手机和电脑互通；访客 Wi-Fi 的客户端隔离会阻止直连。若系统提示局域网访问或防火墙许可，应按实际使用网络授权。
手机界面统一使用上述配对流程，已移除手动地址/token 连接。旧部署需升级电脑端并生成配对码。无需在旧 `[mobile.devices]` 中手工添加二维码配对的手机，也不会自动开启旧的明文上传接口；服务端旧接口暂留兼容。

## 自建中继

配置样例在 `deploy/mobile-relay/`，按 [FRP 官方 TCP 转发方式](https://gofrp.org/en/docs/examples/ssh/) 部署：

- 公网服务器运行 `frps`，使用 `frps.toml.example`，设置 `PEERCARRY_FRP_TOKEN` 为随机密钥。
- 电脑运行 `frpc`，使用 `frpc.toml.example`，设置同一密钥及 `PEERCARRY_RELAY_HOST`。密钥不进入手机配对码。
- 服务器允许 TCP 7000（FRP 控制）和 65197（此电脑的 TLS 转发）。多台电脑分配不同转发端口。
- 先执行 `frps verify -c <配置文件>` 和 `frpc verify -c <配置文件>` 检查配置，再启动对应程序。
- 电脑“连接手机 → 跨网连接”填写 `https://自己的中继域名:65197`，生成新配对码。

FRP 需使用 **TCP 原样转发**，目标为 `127.0.0.1:5197`。不得将 5199 作为此中继目标，也不要在 Nginx、Cloudflare、FRP 插件或其他代理上终止手机这条 TLS 连接。该方案不需要给手机安装 FRP、Tailscale 或其他 VPN。
FRP 控制连接的 TLS 和应用的端到端 TLS 是两层；内容保密性由手机固定的电脑证书保障。外层控制通道如需相互认证，可按 [FRP TLS 文档](https://gofrp.org/en/docs/features/common/network/network-tls/) 配置自己的 CA。
中继仍能看到连接时间、IP 和流量大小，也能拒绝服务；此处的端到端加密只针对传输内容。

## 授权与协议边界

- 二维码包含 256 位一次性随机密钥和电脑证书，有效期 5 分钟；同一二维码只配对一台设备。配对响应丢失时，同一请求可安全重试。
- 配对授予本机共享历史读取、文本发送和文件上传权限。最多 8 台同时授权设备，单个会话最长 24 小时。
- 当前证书和手机授权在内存中。电脑服务或手机应用重启后重新扫码；更换电脑网络或中继地址需停止连接、重新生成。
- 所有路由禁止重定向，手机不使用系统代理或系统 CA 作为替代信任。没有未加密的自动回退。
- 管理接口必须经本机回环地址、合法 Host 和同源 Origin 访问。TLS 手机服务不提供配置、启动程序、AI、自动捕获、自动应用、任意路径文件访问等端点。
- 二进制历史仅传元数据。文件上传保留 1 MiB 分片、SHA-256 校验、偏移冲突恢复、设备隔离和存储配额。
- 配对文件使用独立的 `paired-uploads` 目录；旧上传继续使用 `mobile-uploads`。两者各自应用现有配置中的配额，启用两种方式时应为两者预留空间。
- 原服务找不到 Tailscale 地址时，默认绑定回环地址。已有显式 `network.bind` 配置仍受尊重。

## 验证

Rust：`cargo test -p peercarry-core --test mobile_connect`。

Flutter/Rust 真实 TLS 联调（使用独立测试数据库和合成内容）：

```powershell
# 仓库根目录，使用一个不存在 ticket.json 的新目录
cargo run -p peercarry-core --example mobile_connect_probe -- target/pair-probe-unique
# 另一个终端，在 apps/mobile 目录
$env:PEERCARRY_PAIR_TEST_DIR = 'D:\workspace\sync-clip\target\pair-probe-unique'
flutter test --no-pub test/connection_test.dart
```

探针仅监听本机，模拟两条透明 TCP 通道；验证实际 Dart→Rust TLS 握手、配对、文本、2.5 MB 分片上传，并主动断开“局域网”通道验证中继回退。它不证明公网 FRP 部署或 Android/iPhone 真机网络已验收。

仍需真机验收：相机扫码与局域网权限、真实 Wi-Fi/移动网络切换、锁屏断网恢复、实际 FRP 部署的跨网传输。iOS 构建需在 macOS 上进行。
现有登录协助仍使用独立的专用协议和配对码，本次普通内容中继不承载登录协助。

## 2026-09-22 本地交付记录

- PASS：Rust workspace 124 项测试；最后的文本请求串行化改动另通过真实 HTTPS 定向回归。
- PASS：Flutter 16 项测试，包含真实 Dart/Rust TLS 联调。证书不匹配被拒绝，2.5 MB 文件在第一个 1 MiB 分片后切断直连，剩余分片通过透明 TCP 中继完成。
- SKIP：旧手动地址协议的 Flutter 独立联调测试未配置；Rust 旧上传接口测试仍通过。
- PASS：`flutter analyze --no-pub`、Rust 格式、`git diff --check`；Clippy 成功，保留仓库既有警告，新连接模块无新增警告。
- PASS：真实 Flutter 组件渲染并检查设备页；浏览器检查隔离电脑页面的入口、二维码生成及停止连接。仅使用合成数据，无剪贴板捕获或自动应用。
- PASS：Android debug APK、Windows CLI 和托盘 debug 程序构建成功；APK 中确认包含新扫码和连接路由代码。
- 未验收：Android/iPhone 真机、真实公网 FRP、实际跨网和系统权限流程。FRP 模板按官方文档核对，本机未安装 FRP，未执行 `frpc/frps verify`。

测试产物（相对仓库根目录）：

- `apps/mobile/build/app/outputs/flutter-apk/app-debug.apk`
- `target/debug/peercarry-tray.exe`
- `target/debug/peercarry.exe`

APK SHA-256：`bb09840ceaaf96cfbee843fa5d4773c20b41e49f757460e7a59e12e77d6aa87f`。
这些是本地测试构建，未发布、未安装，未更改防火墙或公网服务器。运行新版桌面端前先退出已有桌面实例，避免共用同一数据库。
