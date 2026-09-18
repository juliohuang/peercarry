# 发布与更新验收记录

验证环境：Windows x64，2026-09-10。应用版本：0.2.0。

## 已验证

- `cargo test --workspace --locked`：68 项测试通过，包括更新签名、下载完整性、版本比较、重定向限制及维护期间请求互斥。
- `python -m unittest discover -s scripts/release -p 'test_*.py'`：6 项测试通过，包含实际调用清单生成/验签命令、篡改拒绝及稳定版本发布顺序。
- `cargo build --release --locked -p peercarry-tray -p peercarry-cli`：通过，CLI 报告 `peercarry 0.2.0`。
- `python scripts/tests/update-helper-e2e.py target/release/peercarry-tray.exe`：正常升级、无效可执行文件回滚、错误版本健康检查超时回滚，三条路径通过。使用独立临时目录、原生测试进程和真实 HTTP 健康检查，不涉及日常剪贴板数据。
- Windows PE 子系统为 GUI（2）；ZIP 完整性及托盘、CLI、安装脚本、说明文件检查通过。
- PowerShell 安装脚本 AST、两个 Shell 脚本语法、工作流 YAML 解析通过。
- `cargo clippy -p peercarry-updater -p peercarry-tray --all-targets --locked`：退出 0，仍有风格建议和既有警告，不属于零警告验收。
- `git diff --check`：通过。原有未提交改动保留，没有提交或推送。

全量测试发现移动上传生命周期测试使用 1 秒 TTL，易受整秒时间戳边界影响。
测试 TTL 调整为 3 秒，等待同步调整为 3.1 秒；没有改变生产 TTL。

## 尚未验证与交付边界

- 未执行真实 GitHub Actions 发布、OSS 上传及国内机器在线下载；需要配置仓库、下载地址和签名密钥。
- 未运行 Windows 安装脚本写注册表或替换用户现有安装；当前运行的旧版本保持原状。
- 未测试 macOS/Linux 实机安装与更新，也未通过 Computer Use 验收托盘菜单。
- 进程测试验证实际更新助手；原有托盘到助手的交接代码已审阅，但尚未通过真实版本服务器完成整条应用内升级。
- 首次需要安装含更新功能的版本；应用内升级仅替换托盘及其内嵌服务，独立 CLI 用完整安装包更新。

## 本地安装包

路径：`target/dist/peercarry-v0.2.0-x86_64-pc-windows-msvc.zip`。
大小：7,364,674 字节。
SHA-256：`9b54700810322d682ae71ab554ca1183fc152fe0326a90a9047b6aa20765d4e3`。
它是本地开发构建，尚未内置实际发布地址和可信公钥。

发布流程与首次安装命令见 [release-publishing.md](release-publishing.md)。
