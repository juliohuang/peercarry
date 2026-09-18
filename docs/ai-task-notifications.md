# AI 任务提醒（0.3.0）

本版为 Codex 和智谱 Z Code 提供轻量 Hook 接入，复用 Tailscale 设备发现和 HTTP 通信。不使用 OCR，也不调用模型识别状态。

## 使用流程

1. 每台参与电脑安装本版，将 `peercarry` 与 `peercarry-tray` 放在同一固定目录。
2. 打开时间线的「AI 任务」页，扫描本机工具，然后点击绑定。绑定会修改当前用户的工具配置，保留其他 Hook，并备份原配置。解绑只移除本工具的处理器。
3. Codex 按工具要求信任 Hook；Z Code 新开会话后验证。扫描成功或绑定成功仅表示配置已找到或已写入，不表示真实会话验证通过。
4. 在各电脑设置相同、至少 32 字符的随机 `ai_token`，保存后重启。不要把令牌放进仓库。已有全局网络认证时仍须满足全局认证；AI 令牌不替代它。
5. 在当前使用的电脑勾选「在本机接收 AI 提醒」。切换电脑时手动关闭旧电脑、开启新电脑。

也可使用 `peercarry ai scan`、`peercarry ai bind codex --dry-run`、`peercarry ai bind zcode`、`peercarry ai bind codex --remove`。安装路径变化后需重新检查绑定。

## 事件与提醒

- `UserPromptSubmit`：开始处理信号。
- `PermissionRequest`：请求授权，提醒用户查看原应用。
- `Stop`：本轮停止信号，不能等同于整个项目完成；其他 Hook 仍可能让工具继续。
- `PostToolUse`：工具执行信号，用于更新状态；不代表任务完成。
- Z Code `PostToolUseFailure`：工具调用失败，不代表整个会话失败。

本机先保存 Hook 元数据，后台约每五秒拉取设备状态。事件按 ID 去重，同一会话更新可撤销尚未显示的旧提醒。离线设备显示未知或不可达。第一版提醒为托盘程序弹窗，不是系统通知中心，也不自动判断用户当前活跃的电脑。

仅保存工具、会话 ID、项目目录名、事件类型和时间，不保存需求文本、代码、工具参数或输出。事件历史最多 1000 条；待处理队列最多 256 个文件或约 8 MiB，超限淘汰旧记录，因此不保证无限离线期间的完整历史。

## 验证与边界

自动检查：`cargo test --workspace --offline`；真实本机进程检查：`python scripts/tests/ai-hooks-e2e.py target/release/peercarry.exe`。

进程测试覆盖离线 Hook 入队、守护进程补发、去重、元数据过滤、接收开关和工具扫描。输入为模拟 Hook 事件；真实 Codex/Z Code 会话、Windows 提醒交互、NUC 双机收发以及 macOS/Linux 尚需验收。普通提问等待、ChatGPT 云端任务和自动跟随活跃电脑不在本版保证范围内。
