# PeerCarry Mobile — M0

Flutter Android-first 客户端。支持手动连接电脑、查看共享历史、显式复制文本、发送文本、选择单个文件并分块上传。接收历史不会自动覆盖剪贴板。

## 连接电脑

手机和电脑先加入同一可信 Tailscale 网络，电脑运行包含移动上传接口的新版本服务。在电脑数据目录的 config.toml 合并以下配置，然后重启服务：

```toml
[mobile]
enabled = true
max_storage_bytes = 8589934592
max_sessions = 128
session_ttl_secs = 86400

[[mobile.devices]]
id = "my-phone"
token = "REPLACE_WITH_A_RANDOM_SECRET_OF_AT_LEAST_32_CHARACTERS"
```

示例 token 是占位符，必须换成随机密钥。在手机填写电脑的 Tailscale HTTP 地址（默认端口 5199）、设备 ID、设备 token。如果电脑配置了 network.auth_token，同时填写共享 token。不要使用公开互联网明文 HTTP；Android 允许 HTTP 是为兼容 Tailscale 内的现有协议。

设备 token 仅授权上传接口；历史和文本接口沿用现有共享 token 策略。该版本没有扫码配对或细粒度历史访问权限。连接信息仅保存在内存。

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
