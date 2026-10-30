# PeerCarry Mobile — M0

Flutter Android-first 客户端。支持扫码连接电脑、局域网优先与加密中继回退、查看共享历史、显式复制文本、发送文本、选择单个文件并分块上传。接收历史不会自动覆盖剪贴板。

## 连接电脑

推荐：电脑时间线打开“连接手机”并生成二维码，手机“设备 → 扫码连接电脑”核对名称后确认。同一局域网直接连接，跨网可使用电脑配置的自建 FRP TCP 中继。手机不需要 Tailscale；HTTPS 证书通过配对码固定，TLS 在电脑终止。

配对码有效期 5 分钟、只用于一台手机。会话最长 24 小时，连接信息仅在内存保留，任一端重启需重扫。电脑支持撤销单台设备和停止全部手机连接。[详细说明及部署模板](../../docs/mobile-connect.md)。

没有相机时，可在电脑复制配对内容，在手机选择“手动输入配对码”。两种入口使用同一套配对与 TLS 连接，不需要填写地址、共享密钥或设备 token。

手机界面已移除旧的手动地址/token 连接。旧部署请升级电脑端并生成新的配对码。桌面服务的旧上传接口暂留兼容；扫码不会开启它，也不使用旧的 `[mobile.devices]` 授权。

登录协助标为实验功能，采用独立配对与授权，仅限合成测试资料；尚未完成真实手机和外部 AI 执行器验收。账号库仍是独立开发组件，未接入应用导航和同步服务。

## 传输与限制

- 1 MiB 分块，客户端流式计算 SHA-256，服务端完整校验后才发布文件条目。
- 上传响应丢失时查询实际偏移；同一客户端会话内重新选择同一文件可续传。应用退出后没有持久化任务恢复。
- 仅前台运行；没有后台传输、暂停按钮、系统分享入口、文件下载或图片预览。
- 服务端重启可恢复会话。过期未完成会话在启动或创建上传时清理；已完成文件仍占配额，不会自动清理。默认最多 128 个会话、8 GiB 总预留空间，单文件还受 limits.max_transfer_bytes 限制。
- 服务端当前串行处理上传磁盘操作和校验；多手机并行吞吐优化留到后续。
- iOS 仅生成工程骨架，尚未配置并验证完整网络和真机行为。

## 开发与验收

在本目录执行 `flutter pub get`、`flutter analyze`、`flutter test`、`flutter build apk --debug`。测试 APK 位于 build/app/outputs/flutter-apk/app-debug.apk。

真实服务联调：在仓库根目录运行 `cargo run -p peercarry-core --example mobile_server_probe -- target/NEW_TEST_DIR`，另一个终端把 PEERCARRY_MOBILE_TEST_CONNECTION 设为其 connection.json 的绝对路径，再执行 `flutter test`。探针仅监听 loopback，使用独立数据库和临时测试凭据；不要发布该连接文件。

本轮验证：Rust workspace 测试通过；Flutter 8 项测试通过（包含真实 Rust 服务文本和 2.5 MB 分块上传）。Android 真机未连接，文件选择器、剪贴板、锁屏断网和手机到电脑 Tailscale 传输需真机验收。
