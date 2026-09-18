# P1 本地固定流程原型

`peercarry-automation` 是一个独立的、按 JSON 定义的本地流程执行器。它不调用模型，也不依赖 Computer Use。新流程默认使用 dry-run；只有调用方同时选择非 dry-run 后端并传入 `execute=true` 才允许输入。

流程目标必须包含可执行文件名和精确、非空窗口标题。控件 selector 必须有 `role`，并且至少有稳定的 `name` 或 `automation_id`；持久化索引、通配符和绝对坐标不是定位主键。提交动作在 P1 中始终拒绝。每步有超时，停止令牌在下一步前生效，断言失败立即停止后续步骤；后端同步调用无法被线程强杀，实际适配器必须把超时传给 UIA 操作并在返回后报告超时。

当前 crate 提供后端 trait、Windows UIA 后端和跨平台 `UnsupportedBackend`。Windows 后端使用 UIA ValuePattern 写入并回读，拒绝控制字符、已有草稿、密码/只读控件和按钮 Invoke；Expand/Selection 仅使用对应 UIA pattern。剪贴板粘贴仍明确返回 Unsupported，不修改系统剪贴板。Linux/macOS 返回 Unsupported。

示例：

```json
{
  "version": 1,
  "name": "focus prompt",
  "target": { "exe": "zcode.exe", "title": "My Project" },
  "steps": [
    { "id": "focus", "action": "focus", "selector": { "role": "edit", "name": "Prompt" } },
    { "id": "check", "action": "assert", "assertion": { "kind": "element_present", "selector": { "role": "edit", "name": "Prompt" } } }
  ]
}
```

P1 尚未声称完成 ZCode 连续输入验收；只读窗口探测已有实机证据，ValuePattern 写入/回读、缩放/多窗口/抢占和连续 20 次流程仍需在 Windows 交互式桌面单独验证。

Windows 上可运行只读探测：`peercarry-automation inspect --exe ZCode.exe --title "ZCode"`。它要求 exe 基名和窗口标题各自精确唯一匹配，只输出有限数量的交互控件 role/name/automation_id，不读取 Value/TextPattern，不执行点击或输入。

验证空白输入和回读时，可由用户在交互式 Windows PowerShell 中手动运行 `powershell.exe -ExecutionPolicy Bypass -File examples/automation/windows-fixture.ps1`，再执行 `fixture-input.json` 流程。fixture 标题为 `peercarry automation fixture`，只包含一个空的 `Automation Prompt` 文本框，没有提交按钮，最多运行 5 分钟后自动关闭。脚本不会由构建或测试自动启动；目标进程因此使用 `powershell.exe`，而不是 ZCode。

native backend 的执行前策略必须同时满足以下条件：

- 每个动作重新确认窗口句柄仍属于目标 exe 和精确标题；目标窗口关闭、标题变化、最小化或出现第二个匹配窗口时立即停止。
- `Focus` 后必须用 UIA focused element/keyboard focus 与目标控件比对；无法确认焦点属于目标控件时拒绝后续键盘或 ValuePattern 写入。前台窗口变化、锁屏、远程桌面断开和权限级别不匹配都应返回明确失败。
- 写入前必须读取目标控件的只读/密码状态和当前值。目标已有草稿时默认拒绝覆盖或追加；不得隐式发送 Ctrl+A、Return 或其他提交快捷键。控制字符（包括换行和制表符）明确拒绝，不进入 UIA ValuePattern。
- 只允许 ValuePattern 写入支持该模式且确认为空的 edit；不支持 ValuePattern 的 contenteditable 或自绘控件应返回 Unsupported，不能退回盲目坐标点击或 SendInput。
- `Expand` 只能作用于支持 ExpandCollapsePattern 的 combo/menu 控件；选择只能作用于支持 SelectionItemPattern 的 list item/menu item。button 的 Invoke/Click 在 P1 中拒绝。
- 一个桌面同一时刻只能有一个执行流程（native backend 已实现桌面互斥）；用户抢占焦点或在步骤间修改内容后，后续步骤必须停止并要求重新确认。锁屏、焦点抢占和远程桌面断开尚未完成全面实测。UIA 调用超时只能标记失败，不能在未知状态下自动重放。

需要真实输入时必须显式执行：`peercarry-automation --execute <flow.json>`；默认命令只做流程校验/dry-run。输入文本中的换行、制表符等控制字符会被拒绝，避免通过 Return 意外提交。

以上策略是 native backend 的实现门槛，不代表 ZCode 已通过真实输入验收。当前证据仅覆盖 Windows 只读窗口/控件探测；输入、回读、菜单选择、草稿保护、焦点抢占和多窗口场景仍需逐项实机记录。
