# 本机剪贴板写入与导出验收

2026-09-09，使用现有 release `peercarry.exe` 与本机运行中的服务验证文本路径。测试使用唯一标记，不发送到其他设备。

| 步骤 | 校验 | 结果 |
| --- | --- | --- |
| 系统剪贴板写入 | 写入含中文、英文、数字的样本并回读 | PASS |
| `peercarry --json push` | 捕获类型为 text，内容与样本完全一致 | PASS |
| `peercarry pull <完整ID>` | 先将剪贴板替换为 sentinel，再通过应用写回；回读与原样本完全一致 | PASS |
| `peercarry show <完整ID>` | 导出的 ID 与文本均与捕获条目一致 | PASS |
| 文件导出 | 保存为 UTF-8 `entry.json`、`clipboard.txt`，重读并独立检查中文 | PASS |

通过的导出目录：`target/clipboard-export-20260909-203525/`。
通过的测试条目 ID：`39471a1c-7ab9-4f2a-9818-906a22a87a21`。

首轮脚本缺少 UTF-8 BOM，被 Windows PowerShell 按旧编码解释，中文样本本身乱码；该轮不计入中文验收。脚本编码修正后完成上述复测。这不是已确认的 peercarry 编码问题。

这里的导出是将 CLI 已有 JSON 输出及文本保存成文件，未新增产品导出按钮。未覆盖图片、文件引用、跨机传输或 ZCode 自动投递。系统剪贴板最终保留正确的测试文本；两轮测试历史条目均保留，没有删除用户历史。测试脚本和导出文件位于被 Git 忽略的 `target/`。
