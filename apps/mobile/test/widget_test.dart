import 'package:flutter_test/flutter_test.dart';
import 'package:flutter/material.dart';
import 'package:peercarry_mobile/main.dart';

void main() {
  testWidgets('renders connection form', (tester) async {
    await tester.pumpWidget(const PeerCarryApp());
    expect(find.text('连接并刷新'), findsOneWidget);
    expect(find.text('选择文件上传'), findsOneWidget);
  });
  testWidgets('invalid address reports error without leaving UI busy',
      (tester) async {
    await tester.binding.setSurfaceSize(const Size(800, 1600));
    addTearDown(() => tester.binding.setSurfaceSize(null));
    await tester.pumpWidget(const PeerCarryApp());
    await tester.tap(find.text('连接并刷新'));
    await tester.pumpAndSettle();
    expect(find.textContaining('连接失败'), findsOneWidget);
    expect(tester.takeException(), isNull);
    await tester.tap(find.text('连接并刷新'));
    await tester.pumpAndSettle();
    expect(tester.takeException(), isNull);
  });
}
