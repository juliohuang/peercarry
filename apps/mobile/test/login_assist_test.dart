import 'dart:convert';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:peercarry_mobile/login_assist_client.dart';
import 'package:peercarry_mobile/login_assist_page.dart';
import 'package:peercarry_mobile/app_theme.dart';
import 'support/localized_app.dart';

void main() {
  testWidgets('pairing screen fits a narrow phone viewport', (tester) async {
    tester.view.physicalSize = const Size(320, 568);
    tester.view.devicePixelRatio = 1;
    addTearDown(tester.view.resetPhysicalSize);
    addTearDown(tester.view.resetDevicePixelRatio);
    await tester.pumpWidget(
        localizedTestApp(theme: carryTheme(), home: const LoginAssistPage()));
    await tester.pumpAndSettle();
    expect(tester.takeException(), isNull);
    expect(find.text('在手机上，继续登录'), findsOneWidget);
    expect(find.text('原型仅限测试资料，请勿输入真实密码或验证码。'), findsOneWidget);
    expect(find.text('配对电脑'), findsOneWidget);
    await tester.pumpWidget(const SizedBox());
  });
  test('pairing requires HTTPS and rejects credential-bearing URLs', () {
    final data = {
      'version': 1,
      'url': 'http://127.0.0.1:5198',
      'certificate': base64Encode([1, 2, 3]),
      'secret': List.filled(64, 'a').join()
    };
    expect(() => LoginTicket.parse(jsonEncode(data)),
        throwsA(isA<LoginAssistException>()));
    data['url'] = 'https://secret@127.0.0.1:5198';
    expect(() => LoginTicket.parse(jsonEncode(data)),
        throwsA(isA<LoginAssistException>()));
    data['url'] = 'https://127.0.0.1:5198';
    expect(LoginTicket.parse(jsonEncode(data)).url.scheme, 'https');
  });
  testWidgets(
      'invalid ticket returns a fixed error without exposing its content',
      (tester) async {
    await tester.pumpWidget(
        localizedTestApp(theme: carryTheme(), home: const LoginAssistPage()));
    await tester.ensureVisible(find.byType(TextField));
    await tester.enterText(
        find.byType(TextField), 'synthetic-sensitive-invalid-ticket');
    await tester.ensureVisible(find.text('配对电脑'));
    await tester.tap(find.text('配对电脑'));
    await tester.pumpAndSettle();
    expect(find.text('无法配对，请检查配对内容后重试'), findsOneWidget);
    expect(
        find.descendant(
            of: find.byType(CarryNotice),
            matching:
                find.textContaining('synthetic-sensitive-invalid-ticket')),
        findsNothing);
    expect(tester.takeException(), isNull);
    await tester.pumpWidget(const SizedBox());
  });

  testWidgets('English pairing screen fits a narrow phone viewport',
      (tester) async {
    tester.view.physicalSize = const Size(320, 568);
    tester.view.devicePixelRatio = 1;
    addTearDown(tester.view.resetPhysicalSize);
    addTearDown(tester.view.resetDevicePixelRatio);
    await tester.pumpWidget(localizedTestApp(
        theme: carryTheme(),
        locale: const Locale('en'),
        home: const LoginAssistPage()));
    await tester.pumpAndSettle();
    expect(find.text('Continue signing in on your phone'), findsOneWidget);
    expect(find.text('Pair computer'), findsOneWidget);
    expect(tester.takeException(), isNull);
    await tester.pumpWidget(const SizedBox());
  });

  testWidgets('pairing screen remains usable with large text and keyboard',
      (tester) async {
    tester.view.physicalSize = const Size(320, 568);
    tester.view.devicePixelRatio = 1;
    addTearDown(tester.view.resetPhysicalSize);
    addTearDown(tester.view.resetDevicePixelRatio);
    await tester.pumpWidget(localizedTestApp(
      theme: carryTheme(),
      builder: (context, child) => MediaQuery(
        data: MediaQuery.of(context).copyWith(
          textScaler: const TextScaler.linear(1.6),
          viewInsets: const EdgeInsets.only(bottom: 220),
        ),
        child: child!,
      ),
      home: const LoginAssistPage(),
    ));
    await tester.pumpAndSettle();
    expect(tester.takeException(), isNull);
    await tester.scrollUntilVisible(find.text('配对电脑'), 100,
        scrollable: find.byType(Scrollable).first);
    await tester.pumpAndSettle();
    expect(tester.takeException(), isNull);
    await tester.pumpWidget(const SizedBox());
  });
  testWidgets('backgrounding clears pairing input', (tester) async {
    await tester.pumpWidget(
        localizedTestApp(theme: carryTheme(), home: const LoginAssistPage()));
    await tester.ensureVisible(find.byType(TextField));
    await tester.enterText(find.byType(TextField), 'synthetic-only');
    tester.binding.handleAppLifecycleStateChanged(AppLifecycleState.inactive);
    await tester.pump();
    expect(tester.widget<TextField>(find.byType(TextField)).controller!.text,
        isEmpty);
    tester.binding.handleAppLifecycleStateChanged(AppLifecycleState.resumed);
    await tester.pumpWidget(const SizedBox());
  });
}
