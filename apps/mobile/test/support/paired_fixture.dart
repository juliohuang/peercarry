import 'dart:convert';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:peercarry_mobile/client.dart';
import 'package:peercarry_mobile/connection_client.dart';

String syntheticPairingPayload() => jsonEncode({
      'kind': 'peercarry-connect',
      'version': 1,
      'host': '测试电脑',
      'lan': ['https://192.168.1.2:5197'],
      'relay': null,
      'certificate': base64Encode([1, 2, 3]),
      'secret': 'a' * 64,
      'expires_at': DateTime.now()
              .add(const Duration(minutes: 5))
              .millisecondsSinceEpoch ~/
          1000,
    });

// UI fixture only: no real certificate, pairing service or network access.
class FakePairedConnection extends Fake implements PairedConnection {
  FakePairedConnection(this.client);
  final PeerCarryClient client;
  @override
  final route = ValueNotifier<ConnectionRoute?>(ConnectionRoute.lan);
  @override
  PeerCarryClient createClient() => client;
  @override
  void close() {
    client.dispose();
  }
}

Future<void> reveal(WidgetTester tester, Finder finder,
    [double delta = 150]) async {
  await tester.scrollUntilVisible(finder, delta,
      scrollable: find.byType(Scrollable).first);
  await tester.pumpAndSettle();
  await Scrollable.ensureVisible(tester.element(finder), alignment: 0.5);
  await tester.pumpAndSettle();
}

Future<void> showDevices(WidgetTester tester) async {
  await tester.tap(find.text('设备'));
  await tester.pumpAndSettle();
}

Future<void> enterPairing(WidgetTester tester, {String? payload}) async {
  await reveal(tester, find.text('手动输入配对码'));
  await tester.tap(find.text('手动输入配对码'));
  await tester.pumpAndSettle();
  await tester.enterText(
      find.descendant(
          of: find.byType(AlertDialog), matching: find.byType(TextField)),
      payload ?? syntheticPairingPayload());
  await tester.tap(find.text('下一步'));
  await tester.pumpAndSettle();
  await tester.pump(const Duration(milliseconds: 300));
  await tester.pumpAndSettle();
}

Future<void> confirmPairing(WidgetTester tester) async {
  await tester.tap(find.text('确认配对'));
  await tester.pumpAndSettle();
}
