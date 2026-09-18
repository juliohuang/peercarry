# PeerCarry · 随传

跨 Tailscale tailnet 的**手动**剪贴板桥接工具。Rust 编写，macOS 菜单栏 / Windows 系统托盘 / Linux 命令行。

---

## 为什么设计成"手动"

自动剪贴板同步在多机环境下会打架：A 复制 → B 收到并写入 → B 的监听器把它当成"新内容"推回 A → A 又写入……来回震荡，最后谁都拿不到正确内容。这正是你遇到的"内容不能完全拷贝过去"的根因。

peercarry 的取舍是：**绝不自动写剪贴板**。每一步都由你触发。

| 动作 | 含义 |
| --- | --- |
| **Capture / push** | 把当前系统剪贴板抓进本机历史 |
| **Pull** | 把某条历史取回到当前剪贴板 |
| **Send** | 把条目推给对端；对端只**记录**，不会自动覆盖它的剪贴板 |
| **Pin** | 固定某条，`clear` 和历史上限都不会动它（`peercarry pin 3`，托盘里显示 📌） |

没有后台剪贴板监听，也就没有冲突。

## 大文件怎么处理

复制文件时剪贴板里本来就只有路径、没有内容，peercarry 沿用这个语义：

| 类型 | 传输方式 |
| --- | --- |
| 文本 ≤ 256 KiB | 随条目一起走 |
| 图片 ≤ 1 MiB | 随条目一起走（内联 PNG） |
| 图片预览 | 捕获时另存一张 128px 缩略图（`storage.thumbnail_px`），按 hash 单独取，不下载原图 |
| 更大的文本 / 图片 | 条目只带 SHA-256，你拉取时才从源机流式下载 |
| 文件 / 目录 | **只存路径和大小**；拉取时才从源机流式传输，目录自动打成 tar 并在对端解包还原 |

所以复制一个 20 GB 的文件夹是瞬时的 —— 网络上跑过去的只是一行路径。

## 架构

```
┌──────────────────────┐        ┌──────────────────────┐
│  mac-mini            │        │  legion (Windows)    │
│  ┌────────────────┐  │ HTTP   │  ┌────────────────┐  │
│  │ tray / CLI     │  │◄──────►│  │ tray / CLI     │  │
│  │ ─────────────  │  │ :5199  │  │ ─────────────  │  │
│  │ HTTP service   │  │        │  │ HTTP service   │  │
│  │ redb + blobs   │  │        │  │ redb + blobs   │  │
│  └────────────────┘  │        │  └────────────────┘  │
└──────────────────────┘        └──────────────────────┘
        ▲                                  ▲
        └──── tailscale status --json ─────┘
              （发现对端，去中心）
```

* **全 P2P**：每台机器既是服务也是客户端，没有中心节点，谁关掉都不影响别人。
* **聚合视图**：UI 把本机历史和所有在线节点的历史合并成一个按时间排序的列表。
* **按需拉取**：列表里只有元数据；点某一条才去它的 `origin.addr` 拿字节。
* **发现方式**：直接跑 `tailscale status --json` 拿 IP、主机名、OS、在线状态，不自建发现服务。

### Crate 结构

| crate | 角色 |
| --- | --- |
| `peercarry-core` | 数据模型、存储（redb）、剪贴板、发现、HTTP 服务与客户端 |
| `peercarry-cli` | `peercarry` 命令行，Linux 主力，其他平台用于脚本化 |
| `peercarry-tray` | 菜单栏 / 托盘应用，同时内嵌 HTTP 服务 |

## 安装

### 共同前提

1. **Tailscale 已安装并登录**到同一个 tailnet —— 本工具靠 `tailscale status --json` 发现节点，没有它就只是个本机剪贴板历史。
2. **Rust 工具链**（stable，edition 2021）。编译约 2～7 分钟（release 开了 LTO）。

```bash
git clone https://github.com/juliohuang/peercarry.git
cd peercarry
cargo build --release
# 产物：target/release/peercarry（命令行）、target/release/peercarry-tray（托盘）
```

> 只想当**中继节点**（收下别人推来的内容、供别人拉取，自己不读写剪贴板）时，只装 `peercarry` 就够，不用编托盘。

---

### macOS

托盘应用自带 HTTP 服务，装完跑它就行。

```bash
sudo mkdir -p /usr/local/bin
sudo cp target/release/peercarry target/release/peercarry-tray /usr/local/bin/

# 登录时自启（写 LaunchAgent 并立即启动）
peercarry install-service

# 或只是一次性地跑：
# open /usr/local/bin/peercarry-tray
```

菜单图标出现后自检：

```bash
peercarry status          # 应看到 tailscale ip 与 daemon running
peercarry peers           # 列出 tailnet 里同样装了本工具的对端
```

卸载：

```bash
peercarry install-service --remove
sudo rm /usr/local/bin/peercarry /usr/local/bin/peercarry-tray
```

若 `install-service` 提示无法激活（某些受限环境），手动执行：

```bash
launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/cn.peercarry.plist
```

> 不要同时在"系统设置 → 登录项"里再加一次，两个实例会因 redb 独占锁互相踢。

---

### Linux

**命令行 / 中继节点**（不需要图形库）：

```bash
mkdir -p ~/.local/bin
cp target/release/peercarry ~/.local/bin/
export PATH="$HOME/.local/bin:$PATH"    # 建议写进 ~/.bashrc / ~/.profile
peercarry install-service                   # 写 systemd 用户单元，跟随图形会话启停
```

服务绑在 `graphical-session.target`：登录桌面时启动并探测 `WAYLAND_DISPLAY` / `DISPLAY`，注销即停——剪贴板只存在于会话里，会话外起来也没意义。

想让它**在无图形会话时也当中继**（SSH 登录后退出也继续跑）：

```bash
sudo loginctl enable-linger $USER   # 允许用户级服务在登出后继续（通常需 sudo）
systemctl --user start peercarry
```

> 无头模式下剪贴板读写不可用，只做存储与转发；`peercarry pull` 会失败，但别人可以从这台机器上拉。

**托盘应用**（可选，需要系统库）：

```bash
sudo apt install libgtk-3-dev libxdo-dev libwebkit2gtk-4.1-dev   # Debian/Ubuntu
cargo build --release -p peercarry-tray
cp target/release/peercarry-tray ~/.local/bin/
```

> 时间线窗口用 webkit2gtk 渲染（macOS/Windows 系统自带）。

运行时需要一个支持 AppIndicator / StatusNotifier 的桌面环境（GNOME 需装 AppIndicator 扩展）。

**复制文件到历史**需要 `xclip`（X11）或 `wl-clipboard`（Wayland），`arboard` 不认自定义 MIME。

自检与卸载：

```bash
peercarry status
systemctl --user status peercarry
peercarry install-service --remove
```

---

### Windows

需要 Rust 的 **MSVC** 工具链（`rustup default stable-msvc`）+ Visual Studio 生成工具。

```powershell
git clone https://github.com/juliohuang/peercarry.git
cd peercarry
cargo build --release
# 产物：target\release\peercarry.exe、target\release\peercarry-tray.exe
```

把两个 exe 放到一个固定目录（例如 `%LOCALAPPDATA%\peercarry`），然后在该目录下：

```powershell
.\peercarry install-service      # 写 HKCU\...\CurrentVersion\Run，登录时启动托盘
.\peercarry status
.\peercarry peers
```

卸载：`.\peercarry install-service --remove`，再删目录。

---

### 装完怎么确认真的能用

两台机器都装好后，在 A 上：

```bash
peercarry push                 # 把当前剪贴板收进本机历史
peercarry send <B 的主机名>     # 推给 B（B 只记录，不会自动覆盖它的剪贴板）
```

在 B 上：

```bash
peercarry list                 # 应能看到 A 那条（前面带 ^ 表示它存在别的机器上）
peercarry pull 1               # 取回到 B 的剪贴板
```

`^` 的含义见[聚合列表里的两个标记](#聚合列表里的两个标记)。

**各平台的验证程度**（照着做之前值得知道）：

| 平台 | 命令行 | 托盘 | 自启 |
| --- | --- | --- | --- |
| macOS | 已实机验证 | 已实机验证 | launchd 已实机验证 |
| Linux (Ubuntu 24.04) | 已实机验证 | 未验证（缺系统库的那台机器没编） | systemd 已实机验证 |
| Windows | 未验证（无可用机器） | 未验证 | 未验证 |

## 命令行用法

```bash
# 启动服务（Linux 上需要常驻；托盘应用已内置）
peercarry serve

# 把当前剪贴板抓进本机历史
peercarry push

# 查看聚合列表（本机 + 所有在线节点）
peercarry list
peercarry list --limit 50 --kind files
peercarry list --local            # 只看本机

# 取回到本机剪贴板
peercarry pull                    # 最新一条
peercarry pull 3                  # 列表第 3 条
peercarry pull 7f3a               # id 前缀

# 搜索全部设备的时间线（预览、正文、文件名、设备名，大小写不敏感）
peercarry search aurora           # 单个词
peercarry search 报告 pdf         # 多个词 AND
peercarry search --json aurora    # JSON 输出

# 固定条目：不被 clear 清掉，也不被历史上限挤出去
peercarry pin 3                   # 列表里标记 *
peercarry pin --off 3             # 取消固定

# 默认：再复制同样的内容会删掉老条目，新的排到最前（已 pin 的条目除外）
peercarry push                    # 第一次：captured text entry ...
peercarry push                    # 重复内容：captured text entry ...（老的那条已被替换）
peercarry dedupe                  # 一次性清理历史里已有的重复（保留每组最新一份，pinned 永不动）

# 推送给对端
peercarry send                    # 广播到所有在线节点
peercarry send legion             # 只发给主机名含 legion 的节点
peercarry send --id 7f3a2c1e nuc  # 重发历史里的某条
peercarry send --file 报告.pdf     # 选择文件直接发送，不经剪贴板
peercarry send --file a.zip b.zip legion   # 多个文件，发给指定节点

# 接收文件夹（从对端拉取的文件落在这里）
peercarry download-dir            # 查看当前文件夹
peercarry download-dir D:\recv    # 修改（目录不存在会自动创建）
peercarry download-dir --reset    # 恢复默认 ~/Downloads/peercarry

# 导出某条图片的预览图（默认落在下载目录下的 thumbs/）
peercarry thumb 3
open "$(peercarry thumb 3)"       # macOS 上直接看一眼

# 诊断
peercarry peers                   # 列出 tailnet 里的桌面节点及其服务状态
peercarry status
peercarry show 3
peercarry delete 3
peercarry clear --yes
peercarry rename MacBookPro       # 改本机显示名（默认用主机名）
peercarry rename --clear          # 改回主机名
peercarry config                  # 打印当前生效的配置
peercarry install-service         # 配置开机自启（launchd / systemd / 注册表）
```

`--json` 全局参数可让 `list` / `show` / `push` / `send` 输出 JSON，方便脚本处理。

### 改机器名

```bash
peercarry rename MacBookPro      # 写入 config.toml 的 node.name
peercarry rename --clear         # 去掉覆盖，回到主机名
```

改完**需要重启服务**才生效（名字是启动时广播的），命令会打印对应平台的那一行重启命令。

两个名字别搞混：

| 看到的地方 | 由谁决定 |
| --- | --- |
| `peercarry status` 的 node、条目列表的 SOURCE、托盘菜单的行首 | peercarry 的 `node.name`（`peercarry rename` 改这个） |
| `peercarry peers` 的 HOST 列 | **Tailscale 的机器名**，peercarry 改不动；要改去 Tailscale 后台或 `tailscale set --hostname` |

改名**不会改写历史**：已经捕获的条目保留当时的名字（历史应该反映它产生时的来源）。本机旧条目仍被认作本机——靠的是 Tailscale node id，不是名字。

### 聚合列表里的两个标记

`peercarry list` 默认把本机历史和所有在线节点的历史合并显示，行首的两个标记说明条目状态：

| 标记 | 含义 |
| --- | --- |
| `*` | 已固定，不会被 `clear` 或历史上限清掉 |
| `^` | 只存在于别的节点上（`delete` / `clear` 只作用于本机历史，对这类条目无效） |

`peercarry delete 3` 命中 `^` 条目时会告诉你它存在哪台机器上，并提示用 `peercarry pin 3` 先在本机留一份。
只看本机历史用 `peercarry list --local`。

## 托盘 / 菜单栏

菜单结构（点击托盘图标本身会先刷新一次再弹菜单）：

```
peercarry: ready
─────────────────
Capture Clipboard    ⌘⇧C
Send to All Peers
Send Files…
─────────────────
Recent · 10 min  (click = clipboard)
mac-mini · hello world          ← 点击直接进剪贴板
─────────────────
Devices · last 12 h
mac-mini (12) ▸                 ← 按设备聚合，每设备最多 12 条
  📌 hello world ▸  Pull / Pinned / Send + 缩略图预览
legion (3) ▸
─────────────────
Open App on Peer ▸ legion · chrome
Open Download Folder
Set Download Folder…
─────────────────
Refresh
Quit
```

两个列表分工不同：

* **Recent · 10 分钟** — 只显示最近 10 分钟内所有设备的新条目，**点击行本身就直接取回到剪贴板**，没有二级菜单；跨设备传文本最常用的场景一步完成。
* **Devices · 按设备** — 每台设备一个子菜单（标题带条目数），收纳该设备 12 小时内的条目；超过 12 小时的历史不再出现在托盘里（CLI 仍然完整可用）。设备按最近活跃排序。

设备列表里每条历史是一个子菜单，展开后是该条目的动作：

```
📌 mac-mini · quarterly report ▸  ┌──────────────────────┐
                                  │ Pull to Clipboard    │
                                  │ ✓ Pinned             │
                                  │ Send to All Peers    │
                                  └──────────────────────┘
```

菜单语言**跟随系统**：系统语言是中文就显示中文，否则英文。想单独指定用环境变量 `PEERCARRY_LANG=zh` 或 `en`；启动时会往日志里写一行 `menu language: zh` 便于确认。

* **Pull to Clipboard** — 取回本机剪贴板。图片和文件在点击时才从源机传输。
* 图片条目的子菜单顶部会显示缩略图（128px，点击时才从源机取，已取过的会缓存）。
* **Pinned** — 勾选即固定（对端的条目会先拷贝一份到本机再固定，之后 `clear` 和历史上限都不动它）。
* **Send to All Peers** — 把这条重新推给所有在线节点。
* **Send Files… / 选择文件发送…** — 弹出系统文件选择框，选中的文件（可多选）直接作为一条 Files 记录广播给所有在线节点，不经过剪贴板。
* **Open Download Folder / 打开接收文件夹** — 在文件管理器里打开接收文件夹；**Set Download Folder… / 设置接收文件夹…** — 选一个新文件夹并立即生效（同时写入 config.toml）。
* **Search Timeline… / 搜索时间线…** — 打开**托盘应用自己的窗口**（内嵌视图，不是浏览器标签页）：搜索框覆盖所有设备的条目——预览、正文、文件名、设备名；按天分组展示，回车或点击即取回到本机剪贴板，Ctrl+P 固定。数据来自本机 daemon 的回环接口，对端访问不了。窗口关掉后托盘照常运行，再点菜单会重新打开。命令行等价物是 `peercarry search`。设 `PEERCARRY_WINDOW=1` 环境变量启动托盘时，窗口随启动直接打开。

### 设置页面

时间线页面顶部有"设置"标签，集中了 config.toml 里的全部配置：节点（显示名、可发现、远程改写/启动权限）、网络（端口、绑定、访问令牌、超时）、限制（内联与传输上限）、存储（历史上限、缩略图、去重、接收/数据目录），以及可远程启动的软件列表（增删改）。

* **保存** 写回 `config.toml`。接收文件夹立即生效，其余字段在服务启动时读取，页面上标了"重启生效"——用"**保存并重启服务**"一键完成：新进程等旧进程释放端口后接管，浏览器会自动等待重启完成。
* 设置接口只对本机回环开放（`auth_token` 本来就只有本地用户能读），对端访问不到。

行首的 📌 表示该条已固定。

> macOS 上剪贴板写入强制在主线程执行，符合 AppKit 的要求；网络与磁盘 I/O 都在 Tokio 后台线程。

## 配置

配置文件在第一次运行时生成：

| 平台 | 路径 |
| --- | --- |
| macOS | `~/Library/Application Support/peercarry/config.toml` |
| Linux | `~/.local/share/peercarry/config.toml` |
| Windows | `%APPDATA%\peercarry\config.toml` |

```toml
[node]
name = "mac-mini"          # 展示名，默认取主机名
discoverable = true
allow_remote_apply = false # 是否允许别的节点直接改写本机剪贴板
allow_remote_launch = false # 是否允许别的节点启动本机注册过的软件（见下文）

[network]
port = 5199
bind = ""                  # 留空则自动绑定 Tailscale IP
auth_token = ""            # 可选共享密钥

[limits]
inline_image_bytes = 1048576        # 1 MiB，超过则只同步引用
inline_text_bytes = 262144          # 256 KiB
max_transfer_bytes = 2147483648     # 2 GiB 单次传输上限

[storage]
history_limit = 500
download_dir = ""          # 留空则用 ~/Downloads/peercarry
data_dir = ""

# 本机注册的可启动软件（供本机和对端按名字拉起）
[[apps]]
name = "chrome"
path = "C:/Program Files/Google/Chrome/Application/chrome.exe"
# args = ["--new-window"]  # 固定参数；对端不能传任何参数
```

拉取的文件落在 `download_dir`，同名文件自动加 ` (1)`、` (2)` 后缀，不会互相覆盖。改这个目录不用手编配置文件：`peercarry download-dir <路径>`，或托盘里的"设置接收文件夹…"（托盘里改完立即生效，无需重启）。

### 远程启动软件

在 A 机器上拉起 B 机器安装的某个软件（比如在笔记本上唤起台式机的 Chrome）。设计上按"远程执行"对待，收得很紧：

| 措施 | 含义 |
| --- | --- |
| 显式注册 | 只有目标机 `[[apps]]` 里登记过的软件才能被拉起；`GET /v1/apps` 只暴露名字，不暴露路径 |
| 名字启动 | 启动请求只能带名字，**路径和参数永远不来自网络**，参数固定用注册时写死的 |
| 默认关闭 | 目标机需 `node.allow_remote_launch = true` 并重启服务，否则对端请求一律 403（本机回环不受影响） |

```bash
# 在目标机上注册（并按需打开 allow_remote_launch）
peercarry apps add chrome "C:\Program Files\Google\Chrome\Application\chrome.exe"
peercarry apps remove chrome

# 查看本机注册了什么、在线对端各提供什么
peercarry apps

# 启动
peercarry launch chrome                     # 本机（走 daemon，GUI 出现在桌面会话）
peercarry launch chrome --on legion         # 拉起 legion 上的 chrome
```

托盘菜单的"打开对端软件"子菜单会列出所有在线对端注册的软件，点一下即在对方机器上启动。

### 目录布局

```
<data_dir>/config.toml
<data_dir>/state.redb        条目元数据
<data_dir>/blobs/ab/cdef…    图片与超大文本
<download_dir>/              从对端拉取的文件
```

## 常驻运行

一条命令完成自动启动配置（macOS 写 LaunchAgent、Linux 写 systemd 用户单元、Windows 写 HKCU Run）：

```bash
peercarry install-service     # 安装并尝试启动
peercarry install-service --remove   # 卸载
```

macOS 优先注册托盘应用（自带服务），Linux 注册 `peercarry serve`。若命令所在会话没有权限直接激活服务（比如通过 SSH 或某些自动化环境），它会打印需要手动执行的一条命令。

各平台生成的配置，供手工管理时参考：

**macOS**（`~/Library/LaunchAgents/cn.peercarry.plist`）：

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>cn.peercarry</string>
  <key>ProgramArguments</key>
  <array><string>/usr/local/bin/peercarry-tray</string></array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key>
  <dict><key>SuccessfulExit</key><false/></dict>
  <key>LimitLoadToSessionType</key><string>Aqua</string>
</dict>
</plist>
```

注册 / 注销：

```bash
launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/cn.peercarry.plist
launchctl bootout gui/$(id -u)/cn.peercarry
```

`KeepAlive` 用 `SuccessfulExit=false` 语义：崩溃自动重启，主动 Quit 不会被拉起。

**Linux**（`~/.config/systemd/user/peercarry.service`）：

服务跟随**图形会话**启停（剪贴板只存在于会话里）：登录桌面时自动启动并探测 `WAYLAND_DISPLAY` / `DISPLAY` / `XAUTHORITY`，注销时停止。生成 `~/.local/bin/peercarry-serve.sh` 作为启动包装。

```ini
[Unit]
Description=peercarry daemon
PartOf=graphical-session.target
After=graphical-session.target

[Service]
ExecStart=%h/.local/bin/peercarry-serve.sh
Restart=on-failure

[Install]
WantedBy=graphical-session.target
```

```bash
systemctl --user daemon-reload
systemctl --user enable --now peercarry
```

补充说明：

* 想在**无图形会话**时也当历史中继节点：`systemctl --user start peercarry`（剪贴板操作不可用，仅转发存储）。
* 桌面托盘需要 `sudo apt install libxdo-dev libgtk-3-dev` 后编译 `peercarry-tray`；仅用 CLI 则不需要。
* 复制文件到历史需要 `xclip`（X11）或 `wl-clipboard`（Wayland）。

**Windows**：写入 `HKCU\...\CurrentVersion\Run`，登录时启动 `peercarry-tray.exe`。

> 注意：同一台机器只注册一种自启方式（命令行注册 **或** 登录项二选一），双实例会因 redb 独占锁互相踢。

## 安全

* 服务默认**绑定 Tailscale IP**，不监听局域网或公网。只有拿不到 Tailscale IP 时才退回 `0.0.0.0`，并会打警告日志。
* Tailscale 已提供 WireGuard 加密与节点认证。`auth_token` 是给多人共享 tailnet 场景的第二道锁，所有节点配成同一个值即可。
* 文件读取接口只接受**条目 id + 序号**，路径从本机历史里查，不接受任意路径参数 —— 对端无法把它变成任意文件读取。
* `/v1/actions/*`（会写剪贴板）默认只接受来自 loopback 的请求。要允许对端直接改写本机剪贴板，需显式打开 `node.allow_remote_apply`。

## 故障排查

```bash
peercarry status       # 节点名、Tailscale IP、端口、daemon 状态、条目数
peercarry peers        # 对端是否在线、是否跑了 daemon
peercarry -vv list     # 打开 debug 日志看聚合过程
```

常见问题：

* **`peercarry peers` 显示 `no daemon`** — 对端在线但没跑 `peercarry serve` / 托盘应用。
* **对端拉不走的条目** — 大概率是条目捕获时没拿到 Tailscale IP，`peercarry status` 里看 `tailscale ip` 那一行是否为 `-`。
* **`no local daemon … store could not be opened`** — redb 会独占锁文件，daemon 运行时 CLI 走 HTTP，不会直接开库；若两种路径都失败，检查是否起了两个 daemon。
* **Linux 复制文件看不到** — 需要 `xclip`（X11）或 `wl-copy/wl-paste`（Wayland），`arboard` 不支持自定义 MIME。
* **拉取文件条目报 `file no longer exists on the machine that captured it`** — 文件条目只存路径引用，字节留在捕获它的那台机器上；原文件在那台机器上被删除或移动（比如放在临时目录里被清理）后，任何节点都无法再拉取这条。拉取会先找来源机器、再回退到实际持有条目的对端，两边都没有就报这个错。

## 路线图

- [x] 图片缩略图预览（托盘子菜单内显示；`peercarry thumb N` 导出）
- [x] 托盘菜单里直接 pin / unpin 的交互入口
- [ ] 全局快捷键直接唤出列表窗口
- [ ] 端到端加密（不依赖 Tailscale 的预共享密钥）

## 许可

MIT
