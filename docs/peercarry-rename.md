# PeerCarry 更名与不兼容说明

项目源码、Rust crate、Flutter 包、应用名称、CLI、构建和发布脚本统一采用 PeerCarry / `peercarry`。Cargo 仓库元数据指向 `https://github.com/juliohuang/peercarry`；这只说明当前配置的仓库地址，不代表外部发布状态已验证。

## 当前标识

- 数据目录只使用 `PEERCARRY_DATA_DIR`；未设置时使用平台默认 `peercarry` 目录。不会读取 `SYNC_CLIP_DATA_DIR`，不会回退到 `sync-clip` 目录，也不会自动迁移旧数据库、配置或下载目录。
- HTTP 使用 `x-peercarry-*` 请求头；旧 `x-syncclip-*` 客户端/服务端不兼容。桌面 hello 与手机 hello 的协议版本为 v2，客户端和服务端必须同步升级，v1 对端会被拒绝。
- CLI 命令为 `peercarry`；安装器不安装或复制 `sclip` 别名。旧 `sclip` 脚本需手动改为 `peercarry`。
- AI Hook binding 使用 `peercarry-ai`。旧 `sync-clip-ai` binding 和既有 Hook 配置不会自动迁移；请按新命令重新配置。
- 安装位置与启动项只使用 PeerCarry 名称和路径。安装器不检测、停止、迁移或清理旧 `sync-clip` 程序、目录或注册项。

## 升级要求

旧程序与新程序不可同时运行。升级前请先手动停止并卸载旧程序及旧服务，再安装 PeerCarry；随后重新配置 `PEERCARRY_DATA_DIR`、CLI 脚本和 `peercarry-ai` Hook。旧数据、配置、下载目录和 bindings 不会自动迁移，需要用户自行决定是否手动搬迁；不要让两个版本同时打开同一个数据库。

所有参与通信的 PeerCarry 桌面端和手机端都必须切换到协议 v2。新客户端不能与仍使用 v1 的旧服务通信；安装包版本号或同一 tailnet 不能替代协议版本检查。

现有自动更新订阅地址不会因为改名自动改变；正式发布仍需核对下载地址、签名公钥和渠道清单。本说明不声明外部仓库、安装包或更新渠道已完成发布验收。手机包标识已更名，旧手机应用数据不会自动迁移。

## 首次公开源码

当前 Git 历史已跟踪 `.mimosa` 和 `.workbuddy` 的本地记录。新增忽略规则不能消除旧提交中的这些文件。公开时建议从审查后的源码新建干净仓库，或先单独审查并清理历史；不要直接镜像推送当前历史。本轮未提交、推送或改写历史。

本轮性能优化集中在 AI 提醒：仅在持久状态变化时写盘，本地连接失败及时显示离线，恢复后清理错误，同批事件去重。提醒关闭时一起清除待提醒队列，保存失败则回滚状态。
