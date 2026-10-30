import 'dart:convert';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_localizations/flutter_localizations.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:http/testing.dart';
import 'package:shared_preferences/shared_preferences.dart';
import 'package:peercarry_mobile/app_localizations.dart';
import 'package:peercarry_mobile/app_theme.dart';
import 'package:peercarry_mobile/client.dart';
import 'package:peercarry_mobile/main.dart';

import 'support/paired_fixture.dart';

Widget localizedTestApp({
  required ThemeData theme,
  required Widget home,
  TransitionBuilder? builder,
}) =>
    MaterialApp(
      theme: theme,
      builder: builder,
      locale: const Locale('zh', 'CN'),
      supportedLocales: AppLocalizations.supportedLocales,
      localizationsDelegates: const [
        AppLocalizations.delegate,
        GlobalMaterialLocalizations.delegate,
        GlobalWidgetsLocalizations.delegate,
        GlobalCupertinoLocalizations.delegate,
      ],
      home: home,
    );

void main() {
  testWidgets('disconnected home routes to connection settings',
      (tester) async {
    SharedPreferences.setMockInitialValues({
      'mobile.language': AppLanguage.simplifiedChinese.name,
    });
    await tester.pumpWidget(const PeerCarryApp());
    await tester.pumpAndSettle();
    expect(find.text('连接一台电脑'), findsOneWidget);
    expect(
        tester.widget<TextField>(find.byKey(const ValueKey('compose'))).enabled,
        isFalse);
    await showDevices(tester);
    expect(find.text('扫码连接电脑'), findsOneWidget);
    expect(find.byKey(const ValueKey('computer-address')), findsNothing);
    expect(find.text('高级：手动地址连接'), findsNothing);
    await enterPairing(tester, payload: 'invalid');
    await reveal(tester, find.text('配对失败，请检查配对码后重试'));
    expect(find.text('配对失败，请检查配对码后重试'), findsOneWidget);
    expect(tester.takeException(), isNull);
  });

  testWidgets('narrow large-text layouts remain usable with keyboard',
      (tester) async {
    await tester.binding.setSurfaceSize(const Size(320, 568));
    addTearDown(() => tester.binding.setSurfaceSize(null));
    await tester.pumpWidget(localizedTestApp(
      theme: carryTheme(),
      builder: (context, child) => MediaQuery(
        data: MediaQuery.of(context).copyWith(
          textScaler: const TextScaler.linear(1.6),
          viewInsets: const EdgeInsets.only(bottom: 220),
        ),
        child: child!,
      ),
      home: const HomePage(),
    ));
    expect(tester.takeException(), isNull);
    await showDevices(tester);
    await enterPairing(tester);
    expect(find.text('确认配对'), findsOneWidget);
    await tester.tap(find.text('取消'));
    await tester.pumpAndSettle();
    expect(tester.takeException(), isNull);
  });

  testWidgets('pairing requires confirmation and failure can be retried',
      (tester) async {
    var attempts = 0;
    await tester.pumpWidget(localizedTestApp(
      theme: carryTheme(),
      home: HomePage(pair: (_) async {
        attempts++;
        throw const FormatException('合成连接失败');
      }),
    ));
    await showDevices(tester);
    await enterPairing(tester);
    expect(attempts, 0);
    await tester.tap(find.text('取消'));
    await tester.pumpAndSettle();
    expect(attempts, 0);
    for (var i = 1; i <= 2; i++) {
      await enterPairing(tester);
      await confirmPairing(tester);
      expect(attempts, i);
      await reveal(tester, find.text('配对失败，请检查配对码后重试'));
      expect(find.text('配对失败，请检查配对码后重试'), findsOneWidget);
    }
    expect(tester.takeException(), isNull);
  });

  testWidgets('history stays manual and draft survives tab navigation',
      (tester) async {
    await tester.binding.setSurfaceSize(const Size(430, 1000));
    addTearDown(() => tester.binding.setSurfaceSize(null));
    final clipboardWrites = <String>[];
    final sent = <String>[];
    tester.binding.defaultBinaryMessenger.setMockMethodCallHandler(
      SystemChannels.platform,
      (call) async {
        if (call.method == 'Clipboard.setData') {
          clipboardWrites.add((call.arguments as Map)['text'] as String);
        }
        return null;
      },
    );
    addTearDown(() => tester.binding.defaultBinaryMessenger
        .setMockMethodCallHandler(SystemChannels.platform, null));
    final transport = MockClient((request) async {
      if (request.url.path == '/v1/hello') {
        return http.Response(
            jsonEncode({
              'protocol': 2,
              'host': '测试电脑',
              'version': '0.3.1',
              'capabilities': ['mobile-upload-v1'],
            }),
            200,
            headers: {'content-type': 'application/json; charset=utf-8'});
      }
      if (request.method == 'POST') {
        final data = jsonDecode(request.body) as Map;
        sent.add(data['text'] as String);
        return http.Response(jsonEncode({'id': data['id']}), 200);
      }
      return http.Response(
          jsonEncode([
            {'id': 'text-1', 'kind': 'text', 'text': '合成共享内容'},
            {'id': 'file-1', 'kind': 'files', 'preview': '示例文件.pdf'},
          ]),
          200,
          headers: {'content-type': 'application/json; charset=utf-8'});
    });
    await tester.pumpWidget(localizedTestApp(
      theme: carryTheme(),
      home: HomePage(
          pair: (_) async => FakePairedConnection(PeerCarryClient(
              baseUrl: 'https://paired.peercarry.invalid',
              deviceId: 'synthetic-phone',
              transport: transport))),
    ));
    await showDevices(tester);
    await enterPairing(tester);
    await confirmPairing(tester);
    expect(find.text('测试电脑'), findsOneWidget);
    expect(clipboardWrites, isEmpty);
    expect(sent, isEmpty);

    await tester.enterText(find.byKey(const ValueKey('compose')), '保留这份草稿');
    await showDevices(tester);
    await tester.tap(find.text('传输'));
    await tester.pumpAndSettle();
    expect(
        tester
            .widget<TextField>(find.byKey(const ValueKey('compose')))
            .controller!
            .text,
        '保留这份草稿');
    await reveal(tester, find.text('发送文本'));
    await tester.tap(find.text('发送文本'));
    await tester.pumpAndSettle();
    expect(sent, ['保留这份草稿']);
    expect(clipboardWrites, isEmpty);

    await reveal(tester, find.byTooltip('复制到剪贴板'));
    await tester.tap(find.byTooltip('复制到剪贴板'));
    await tester.pumpAndSettle();
    expect(clipboardWrites, ['合成共享内容']);
    await reveal(tester, find.widgetWithText(ChoiceChip, '文件'), -150);
    await tester.tap(find.widgetWithText(ChoiceChip, '文件'));
    await tester.pumpAndSettle();
    expect(find.byTooltip('复制到剪贴板'), findsNothing);
    expect(find.text('示例文件.pdf'), findsOneWidget);

    await showDevices(tester);
    await reveal(tester, find.text('断开连接'));
    await tester.tap(find.text('断开连接'));
    await tester.pumpAndSettle();
    await tester.tap(find.text('传输'));
    await tester.pumpAndSettle();
    expect(find.text('连接一台电脑'), findsOneWidget);
    expect(find.text('合成共享内容'), findsNothing);
    expect(tester.takeException(), isNull);
  });
}
