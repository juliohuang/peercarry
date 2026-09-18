import 'package:file_selector/file_selector.dart';
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import 'client.dart';
import 'models.dart';

void main() => runApp(const PeerCarryApp());

class PeerCarryApp extends StatelessWidget {
  const PeerCarryApp({super.key});

  @override
  Widget build(BuildContext context) => MaterialApp(
        title: 'PeerCarry',
        theme: ThemeData(colorSchemeSeed: Colors.indigo, useMaterial3: true),
        home: const HomePage(),
      );
}

class HomePage extends StatefulWidget {
  const HomePage({super.key});

  @override
  State<HomePage> createState() => _HomePageState();
}

class _HomePageState extends State<HomePage> {
  final base = TextEditingController();
  final shared = TextEditingController();
  final device = TextEditingController(text: 'mobile');
  final deviceToken = TextEditingController();
  final text = TextEditingController();

  SyncClient? client;
  Hello? hello;
  List<Entry> entries = const [];
  String message = '';
  bool busy = false;
  double? progress;
  int _generation = 0;

  @override
  void dispose() {
    client?.dispose();
    for (final controller in [base, shared, device, deviceToken, text]) {
      controller.dispose();
    }
    super.dispose();
  }

  void note(String value) {
    if (mounted) setState(() => message = value);
  }

  void invalidateSettings() {
    _generation++;
    client?.dispose();
    client = null;
    hello = null;
    entries = const [];
    if (mounted) setState(() {});
  }

  Future<void> connect() async {
    final generation = ++_generation;
    client?.dispose();
    client = null;
    hello = null;
    entries = const [];
    if (mounted) {
      setState(() {
        busy = true;
        message = '';
      });
    }
    SyncClient? candidate;
    try {
      candidate = SyncClient(
        baseUrl: base.text,
        sharedToken: shared.text,
        deviceId: device.text,
        deviceToken: deviceToken.text,
      );
      final connectedHello = await candidate.hello();
      final connectedEntries = await candidate.entries();
      if (!mounted || generation != _generation) {
        candidate.dispose();
        return;
      }
      client = candidate;
      hello = connectedHello;
      entries = connectedEntries;
      message = '已连接：${connectedHello.host}';
    } catch (error) {
      candidate?.dispose();
      if (mounted && generation == _generation) message = '连接失败：$error';
    } finally {
      if (mounted && generation == _generation) setState(() => busy = false);
    }
  }

  Future<void> refreshEntries(SyncClient current) async {
    final refreshed = await current.entries();
    if (mounted && identical(client, current)) {
      setState(() => entries = refreshed);
    }
  }

  Future<void> send() async {
    final current = client;
    final value = text.text;
    if (current == null) return note('请先连接');
    if (value.trim().isEmpty) return note('请输入文本');
    if (mounted) setState(() => busy = true);
    try {
      await current.sendText(value);
      if (!mounted || !identical(client, current)) return;
      text.clear();
      note('文本已发送');
      try {
        await refreshEntries(current);
      } catch (_) {
        note('文本已发送，历史刷新失败');
      }
    } catch (error) {
      note('发送失败：$error');
    } finally {
      if (mounted) setState(() => busy = false);
    }
  }

  Future<void> chooseFile() async {
    final current = client;
    if (current == null) return note('请先连接');
    if (!(hello?.capabilities.contains('mobile-upload-v1') ?? false)) {
      return note('电脑未开启移动上传能力');
    }
    if (mounted) {
      setState(() {
        busy = true;
        progress = 0;
      });
    }
    try {
      final file = await openFile();
      if (file == null || !mounted) return;
      await current.upload(file, onProgress: (sent, total) {
        if (mounted && identical(client, current)) {
          setState(() => progress = total == 0 ? 0 : sent / total);
        }
      });
      if (!mounted || !identical(client, current)) return;
      note('文件已上传（仅前台，会话内重试）');
      try {
        await refreshEntries(current);
      } catch (_) {
        note('文件已上传，历史刷新失败');
      }
    } catch (error) {
      note('上传失败：$error');
    } finally {
      if (mounted) {
        setState(() {
          busy = false;
          progress = null;
        });
      }
    }
  }

  @override
  Widget build(BuildContext context) => Scaffold(
        appBar: AppBar(title: const Text('PeerCarry')),
        body: ListView(
          padding: const EdgeInsets.all(16),
          children: [
            TextField(
              controller: base,
              enabled: !busy,
              onChanged: (_) => invalidateSettings(),
              decoration: const InputDecoration(
                labelText: '电脑地址（http/https）',
                hintText: '例如 http://Tailscale-IP:5199',
              ),
            ),
            TextField(
              controller: shared,
              enabled: !busy,
              obscureText: true,
              onChanged: (_) => invalidateSettings(),
              decoration: const InputDecoration(labelText: '共享 token（可空）'),
            ),
            TextField(
              controller: device,
              enabled: !busy,
              onChanged: (_) => invalidateSettings(),
              decoration: const InputDecoration(labelText: '移动 device id'),
            ),
            TextField(
              controller: deviceToken,
              enabled: !busy,
              obscureText: true,
              onChanged: (_) => invalidateSettings(),
              decoration: const InputDecoration(labelText: 'device token'),
            ),
            const SizedBox(height: 12),
            FilledButton(
              onPressed: busy ? null : connect,
              child: const Text('连接并刷新'),
            ),
            if (hello != null) Text('已连接：${hello!.host}  ${hello!.version}'),
            const Text('仅前台传输；连接信息和重试记录仅在本次会话保留。'),
            const Divider(height: 28),
            TextField(
              controller: text,
              enabled: client != null && !busy,
              maxLines: 4,
              maxLength: 65536,
              decoration: const InputDecoration(
                labelText: '发送文本',
                border: OutlineInputBorder(),
              ),
            ),
            Row(
              children: [
                Expanded(
                  child: FilledButton(
                    onPressed: client != null && !busy ? send : null,
                    child: const Text('发送'),
                  ),
                ),
                const SizedBox(width: 8),
                Expanded(
                  child: OutlinedButton(
                    onPressed: client != null && !busy ? chooseFile : null,
                    child: const Text('选择文件上传'),
                  ),
                ),
              ],
            ),
            if (progress != null) LinearProgressIndicator(value: progress),
            if (message.isNotEmpty)
              Padding(
                padding: const EdgeInsets.symmetric(vertical: 8),
                child: Text(message),
              ),
            const Text('历史记录',
                style: TextStyle(fontSize: 18, fontWeight: FontWeight.bold)),
            ...entries.map(
              (entry) => Card(
                child: ListTile(
                  title: Text(entry.display,
                      maxLines: 3, overflow: TextOverflow.ellipsis),
                  subtitle: Text(entry.kind),
                  trailing: entry.isText
                      ? IconButton(
                          icon: const Icon(Icons.copy),
                          tooltip: '复制（需手动点击）',
                          onPressed: () async {
                            await Clipboard.setData(
                                ClipboardData(text: entry.text!));
                            note('已复制到剪贴板');
                          },
                        )
                      : null,
                ),
              ),
            ),
          ],
        ),
      );
}
