import 'dart:convert';
import 'dart:io';
import 'package:file_selector/file_selector.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:http/testing.dart';
import 'package:peercarry_mobile/client.dart';

void main() {
  test('hello requires the current PeerCarry protocol and headers', () async {
    for (final version in [1, 2, 3]) {
      final client = PeerCarryClient(
          baseUrl: 'http://localhost',
          deviceId: 'test',
          sharedToken: 'synthetic-shared',
          transport: MockClient((request) async {
            expect(request.headers['x-peercarry-token'], 'synthetic-shared');
            expect(request.headers.containsKey('x-syncclip-token'), isFalse);
            return http.Response(jsonEncode({'protocol': version}), 200);
          }));
      try {
        if (version == 2) {
          await client.hello();
        } else {
          await expectLater(client.hello(), throwsFormatException);
        }
      } finally {
        client.dispose();
      }
    }
  });
  test('lost PATCH response recovers confirmed offset without retransmission',
      () async {
    final dir = await Directory.systemTemp.createTemp('upload-test-');
    final file = await File('${dir.path}/test.bin').writeAsBytes([1, 2, 3]);
    const id = '12345678-1234-4234-8234-123456789abc';
    var offset = 0;
    var patches = 0;
    final transport = MockClient((r) async {
      expect(r.followRedirects, isFalse);
      expect(r.headers['x-peercarry-device-token'], 'synthetic-token');
      expect(
          r.headers.keys.any((key) => key.startsWith('x-syncclip-')), isFalse);
      if (r.method == 'PATCH') {
        patches++;
        expect(r.bodyBytes, [1, 2, 3]);
        offset = 3;
        throw http.ClientException('lost response');
      }
      final done = r.url.path.endsWith('/complete');
      return http.Response(
          jsonEncode({
            'id': id,
            'size': 3,
            'offset': offset,
            'state': done ? 'completed' : 'uploading',
            'entry_id': done ? id : null
          }),
          200);
    });
    final client = PeerCarryClient(
        baseUrl: 'http://localhost',
        deviceId: 'test',
        deviceToken: 'synthetic-token',
        transport: transport);
    try {
      expect((await client.upload(XFile(file.path))).state, 'completed');
      expect(patches, 1);
    } finally {
      client.dispose();
      await dir.delete(recursive: true);
    }
  });
  test('rejects credential-bearing or path-bearing connection URLs', () {
    for (final url in [
      'http://user:secret@localhost',
      'http://localhost/path',
      'http://localhost?token=x'
    ]) {
      expect(() => PeerCarryClient(baseUrl: url, deviceId: 'test'),
          throwsFormatException);
    }
  });
  test('rejects blank and oversized UTF8 text before network', () async {
    final client = PeerCarryClient(
        baseUrl: 'http://localhost',
        deviceId: 'test',
        transport: MockClient((_) async => throw StateError('must not send')));
    try {
      await expectLater(client.sendText('   '), throwsFormatException);
      await expectLater(client.sendText(List.filled(22000, '中').join()),
          throwsFormatException);
    } finally {
      client.dispose();
    }
  });
}
