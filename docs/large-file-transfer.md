# 大文件 P2P 传输与验收

## 本次行为

- 文件仍由目标电脑按需拉取，不自动覆盖剪贴板。
- 普通 API 保留原有总超时；文件请求单独等待响应准备（默认 600 秒），传输中仅检查连续无数据时间（默认 60 秒）。持续有数据的下载可以超过 30 秒。
- 普通文件支持单段 HTTP Range 与强 SHA-256 ETag。中断保留 `.peercarry-*.part` 和 `.meta`，同一来源、条目和下载目录重试时发送 Range/If-Range。
- 来源变化或不支持 Range 的旧服务返回 200 时重新下载，避免拼接新旧内容。412/416 或哈希错误最多自动重启一次。
- 校验长度、传输上限和强 ETag 对应的完整 SHA-256，落盘后才发布最终文件。最终文件通过硬链接创建，避免覆盖已有文件；文件系统不支持硬链接时明确报错并保留临时文件。
- 同一下载使用进程内与跨进程锁；`.lock` 文件会保留以避免锁文件删除带来的竞争。
- 目录仍先下载 tar，再解包到临时目录；逐项检查展开大小，拒绝链接和特殊文件。目录目前不支持断点续传。
- 保留原始网络、校验和磁盘错误，不再把所有下载失败显示成“源文件已删除”。

## 配置

现有配置兼容，省略新字段时使用默认值：

```toml
[network]
transfer_prepare_timeout_secs = 600
transfer_idle_timeout_secs = 60
```

`limits.max_transfer_bytes` 仍生效（默认 2 GiB）；更大的文件需要在参与传输的两端明确调整上限。更大的准备超时用于发送前的整文件哈希、目录打包等工作。

## 实测结果（2026-09-09）

测试通过独立 `transfer_probe` 示例调用真实的 core HTTP 服务和 PeerClient，不读取剪贴板，不使用正式数据库。Windows 本机与 Linux NUC 通过现有免密 SSH 配置测试，未安装或替换正式程序。

| 测试 | 结果 |
| --- | --- |
| 本机回环 64 MiB | PASS，0.163 秒，392.1 MiB/s，完整 SHA-256 一致 |
| NUC → Windows 1 GiB | PASS，98.518 秒，10.39 MiB/s，完整 SHA-256 一致 |
| NUC → Windows 中断后续传 | 5 秒取消，只有 775096 字节 `.part`、无正式文件；重试请求 `bytes=775096-`，服务返回 206；84.363 秒完成，完整 SHA-256 一致 |
| Windows → NUC 64 MiB | PASS，10.389 秒，6.16 MiB/s，完整 SHA-256 一致 |
| Tailscale 路径 | `tailscale ping` 确认直连；没有使用 DERP 中继 |

单次样本不能证明相对旧版的加速比例。时间包括响应准备、下载、客户端哈希与落盘；探针随后还会独立核验一次 SHA-256。续传耗时不含第一次中断前的时间，不应按完整文件大小计算新增网络吞吐。回环测试不是局域网测速。

上述双机运行后，补充了目录展开限制和严格 ETag 格式检查；普通文件的数据流路径没有改动，最终版本另跑回归测试。

## 自动验证与复现

```text
cargo test --workspace
cargo test -p peercarry-core
cargo clippy -p peercarry-core --all-targets
cargo build -p peercarry-core --example transfer_probe --release
```

Windows 工作区测试通过；core 最终 15 项测试通过，NUC Linux release 模式 16 项测试通过（多一项 Linux 平台测试）。覆盖真实 HTTP 中断和取消、Range 续传、来源缩小、错误哈希、大小限制、归档限制及发送端响应。Clippy 通过但仍报告原有的排序与 Default 风格建议。

发送端使用新建的独立目录与空闲端口：

```text
transfer_probe serve TEST_DATA_DIR TAILSCALE_IP:51990 1024
```

根据输出的 `url`、`id`、`sha256`，在接收端执行：

```text
transfer_probe fetch URL ID DOWNLOAD_DIR SHA256
transfer_probe interrupt URL ID ANOTHER_DOWNLOAD_DIR SHA256 5000
transfer_probe fetch URL ID ANOTHER_DOWNLOAD_DIR SHA256
```

探针只应绑定 loopback 或测试机器的 Tailscale 地址。测试完成后停止进程。测试生成的数据位于仓库忽略的 `target/transfer-*` 和 NUC 的独立 `/tmp/peercarry-transfer.oSMugi`，未触碰正式配置和剪贴板。

## 下一步

当前优先解决超时、中断恢复和完整性。单文件仍是一个流，发送前会额外扫描整文件计算哈希；目录仍使用完整 tar。后续可在实测瓶颈明确后加入有限并发、可复用的文件清单和目录逐文件续传。macOS、不同文件系统以及公网两地网络尚未实测。
