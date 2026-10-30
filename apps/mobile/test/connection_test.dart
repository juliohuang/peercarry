import 'dart:convert';
import 'dart:io';

import 'package:file_selector/file_selector.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:peercarry_mobile/connection_client.dart';

Map<String, dynamic> ticket() => {
      'kind': 'peercarry-connect',
      'version': 1,
      'host': '合成测试电脑',
      'lan': ['https://192.168.1.2:5197'],
      'relay': 'https://relay.example.com:65197',
      'certificate': base64Encode([1, 2, 3]),
      'secret': 'a' * 64,
      'expires_at': DateTime.now()
              .add(const Duration(minutes: 5))
              .millisecondsSinceEpoch ~/
          1000,
    };

void main() {
  test('pairing requires scoped HTTPS origins and unexpired one-time secret',
      () {
    final parsed = ConnectionTicket.parse(jsonEncode(ticket()));
    expect(parsed.host, '合成测试电脑');
    expect(parsed.lan.single.host, '192.168.1.2');
    for (final changes in [
      {'kind': 'login-assist'},
      {'version': 2},
      {'secret': 'short'},
      {'expires_at': 1},
      {'certificate': ''},
      {'relay': 'http://relay.example.com'},
      {'relay': 'https://u:p@relay.example.com'},
      {'relay': 'https://relay.example.com/redirect'},
      {
        'lan': ['https://evil.example.com']
      },
      {
        'lan': ['http://192.168.1.2']
      },
      {
        'lan': ['https://8.8.8.8']
      },
      {'lan': [], 'relay': null},
    ]) {
      expect(
          () => ConnectionTicket.parse(jsonEncode({...ticket(), ...changes})),
          throwsFormatException);
    }
  });

  final directory = Platform.environment['PEERCARRY_PAIR_TEST_DIR'];
  test(
      'real pinned TLS: LAN pairing, text, chunks and encrypted relay fallback',
      () async {
    final root = Directory(directory!);
    final data = await File('${root.path}/ticket.json').readAsString();
    final wrongCertificate = jsonDecode(data) as Map<String, dynamic>;
    wrongCertificate['certificate'] = base64Encode(
        await File('${root.path}/unrelated-certificate.der').readAsBytes());
    await expectLater(
        PairedConnection.pair(
            ConnectionTicket.parse(jsonEncode(wrongCertificate))),
        throwsA(isA<http.ClientException>()));
    final connection =
        await PairedConnection.pair(ConnectionTicket.parse(data));
    final client = connection.createClient();
    try {
      expect(connection.route.value, ConnectionRoute.lan);
      expect((await client.hello()).capabilities, contains('mobile-upload-v1'));
      await client.sendText('合成文本：局域网端到端 TLS');
      final file = File('${root.path}/synthetic.bin');
      await file.writeAsBytes(List.generate(2500000, (i) => i % 251));
      var switched = false;
      final first =
          await client.upload(XFile(file.path), onProgress: (sent, total) {
        if (switched || sent < 1024 * 1024) return;
        switched = true;
        // Pause only this isolated test callback while the separate Rust
        // process closes the LAN channel between two upload chunks.
        File('${root.path}/disable-lan').writeAsStringSync('test');
        for (var i = 0; i < 100; i++) {
          if (File('${root.path}/lan-disabled').existsSync()) break;
          sleep(const Duration(milliseconds: 50));
        }
      });
      expect(first.state, 'completed');
      expect(first.offset, 2500000);
      expect(switched, isTrue);
      expect(connection.route.value, ConnectionRoute.relay);
      expect(await File('${root.path}/lan-disabled').exists(), isTrue);
      await client.sendText('合成文本：断开局域网后使用加密中继');
      expect(connection.route.value, ConnectionRoute.relay);
      final second = await client.upload(XFile(file.path));
      expect(second.state, 'completed');
      expect(second.offset, 2500000);
      final entries = await client.entries();
      expect(entries.any((e) => e.id == second.entryId), isTrue);
      expect(entries.any((e) => e.text == '合成文本：断开局域网后使用加密中继'), isTrue);
      // Neither endpoint can authorize another request from the same used QR.
      await expectLater(PairedConnection.pair(ConnectionTicket.parse(data)),
          throwsFormatException);
    } finally {
      client.dispose();
      await File('${root.path}/stop').writeAsString('test complete');
    }
  },
      skip: directory == null
          ? 'Set PEERCARRY_PAIR_TEST_DIR to an isolated mobile_connect_probe directory'
          : false);
}
