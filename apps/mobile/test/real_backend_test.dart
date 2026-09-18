import 'dart:convert';
import 'dart:io';
import 'package:file_selector/file_selector.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:peercarry_mobile/client.dart';

void main() {
  final connection = Platform.environment['PEERCARRY_MOBILE_TEST_CONNECTION'];
  test('real Rust service accepts text and multi-chunk file', () async {
    final config = jsonDecode(await File(connection!).readAsString()) as Map;
    final client = SyncClient(
        baseUrl: config['url'] as String,
        deviceId: config['device_id'] as String,
        deviceToken: config['device_token'] as String);
    final directory =
        await Directory.systemTemp.createTemp('peercarry-mobile-');
    try {
      expect((await client.hello()).capabilities, contains('mobile-upload-v1'));
      await client.sendText('手机联调测试 ${DateTime.now().microsecondsSinceEpoch}');
      final file = File('${directory.path}/sample.bin');
      await file.writeAsBytes(List.generate(2500000, (i) => i % 251));
      final result = await client.upload(XFile(file.path));
      expect(result.state, 'completed');
      expect(result.offset, 2500000);
      expect(
          (await client.entries()).any((e) => e.id == result.entryId), isTrue);
    } finally {
      client.dispose();
      await directory.delete(recursive: true);
    }
  },
      skip: connection == null
          ? 'Set PEERCARRY_MOBILE_TEST_CONNECTION to isolated probe connection.json'
          : false);
}
