# PeerCarry Flutter 界面更新

日期：2026-09-22。设计使用内置 imagegen；实现为 Flutter 原生组件，未将设计稿作为背景图片。后续连接改造已把设备页改为扫码优先，并重新渲染预览；详见 [免 Tailscale 连接](../mobile-connect.md)。

## 设计与实现

- [概念设计图](mobile-ui-concept.png) 与 [完整生成提示词](mobile-ui-prompt.md)。概念图中的内容、连接状态仅为示例。
- 暖白背景、墨绿主色、圆角白卡和统一的输入框/按钮/状态提示。
- 传输页：连接状态、文本发送、文件上传、历史分类、显式复制、空状态和上传进度。
- 设备页：扫码添加电脑、手动输入配对码、连接方式状态、断开、实验性登录协助；已移除旧的地址和密钥配置，所有传输入口使用相同的配对协议。
- 登录协助：扫码优先、手动配对、目标与状态、画面操作、私密填写和交还分组；保留原型限制和现有确认操作。
- 不添加自动剪贴板同步、后台上传或外部 AI 自动恢复。

## Flutter 实际渲染预览

以下图片由 `apps/mobile/tool/render_ui_test.dart` 渲染真实组件，使用合成数据和本地中文字体；不是手机设备截图，未连接真实电脑或账号。

- [传输页：已连接示例](mobile-ui-home-connected.png)
- [传输页：未连接](mobile-ui-home-empty.png)
- [设备页](mobile-ui-devices.png)
- [登录协助](mobile-ui-login.png)

复现预览（在 `apps/mobile` 运行；字体与输出路径按本机调整）：

```powershell
$env:PEERCARRY_UI_OUTPUT='D:\workspace\sync-clip\docs\design'
$env:PEERCARRY_UI_FONT='C:\Windows\Fonts\msyh.ttc'
flutter test --no-pub tool/render_ui_test.dart
```

## 初次界面更新的验证

- `flutter test --no-pub`：14 项通过，1 项真实后端测试因未配置隔离连接而跳过。
- `flutter analyze --no-pub`：无问题。
- 320 × 568、1.6 倍字号、220 高键盘区域：传输/设备和配对页布局测试通过。
- 合成 HTTP 测试验证连接失败可重试、切换页面保留草稿、发送后刷新、历史分类、仅点击后复制、修改连接设置清除旧历史。
- 预览渲染测试通过并人工查看；中文字体仅用于预览，未加入应用依赖。
- `flutter build apk --debug --no-pub`：成功，产物 `apps/mobile/build/app/outputs/flutter-apk/app-debug.apk`。
- 本轮未执行 Android/iOS 真机、相机扫码或跨设备网络验收，未发布或安装应用。

## 2026-09-26 功能收敛验证

- 移除手机端手动地址/token 表单、对应控制器及单独连接流程；扫码和输入配对内容统一使用 TLS 配对。桌面旧上传接口暂留兼容。
- 登录协助入口标为“实验”，账号库仍保持独立开发状态；未将普通手机配对授权扩展到这两项功能。
- Flutter 回归：25 项通过，2 项因未配置隔离服务而跳过（真实 Rust 后端、真实 TLS/中继回退）。新增或调整测试覆盖无效配对码、用户取消、配对失败重试、小屏大字与键盘、草稿保留、显式复制、断开后清除历史。
- `dart analyze lib test tool`：无问题。渲染脚本测试通过，已更新并查看设备页和已连接传输页预览；图片使用合成连接与历史。
- 本轮未构建或发布安装包，未执行真机摄像头、跨设备网络或公网中继验收；历史构建记录不代表本轮版本。

## 修改边界

初次界面更新仅修改 Flutter 界面、相关测试、主题与设计资料。后续免 Tailscale 改造新增独立 TLS 传输和扫码配对，保留登录协助独立协议和已有未提交业务改动。
