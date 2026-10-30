import 'dart:convert';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:http/testing.dart';
import 'package:peercarry_mobile/app_localizations.dart';
import 'package:peercarry_mobile/client.dart';
import 'package:peercarry_mobile/main.dart';
import 'package:shared_preferences/shared_preferences.dart';

import 'support/paired_fixture.dart';

class _FailingPreferenceStore implements LanguagePreferenceStore {
  const _FailingPreferenceStore();

  @override
  Future<String?> load() => Future.error(StateError('private details'));

  @override
  Future<bool> save(String languageName) =>
      Future.error(StateError('private details'));
}

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();

  test('system locale resolves Chinese and falls back to English', () {
    expect(AppLocalizations.resolve(const Locale('zh', 'CN')),
        const Locale('zh', 'CN'));
    expect(AppLocalizations.resolve(const Locale('zh', 'TW')),
        const Locale('zh', 'CN'));
    expect(
        AppLocalizations.resolve(const Locale('en', 'US')), const Locale('en'));
    expect(
        AppLocalizations.resolve(const Locale('ja', 'JP')), const Locale('en'));
    expect(AppLocalizations.resolve(null), const Locale('en'));
  });

  testWidgets(
      'language choice persists and changing it keeps connection and draft',
      (tester) async {
    SharedPreferences.setMockInitialValues({
      'mobile.language': AppLanguage.simplifiedChinese.name,
    });
    final transport = MockClient((request) async {
      if (request.url.path == '/v1/hello') {
        return http.Response(
          jsonEncode({
            'protocol': 2,
            'host': 'Test computer',
            'version': '0.3.1',
            'capabilities': [],
          }),
          200,
          headers: {'content-type': 'application/json; charset=utf-8'},
        );
      }
      return http.Response('[]', 200,
          headers: {'content-type': 'application/json; charset=utf-8'});
    });
    await tester.pumpWidget(PeerCarryApp(
      pair: (_) async => FakePairedConnection(PeerCarryClient(
        baseUrl: 'https://paired.peercarry.invalid',
        deviceId: 'synthetic-phone',
        transport: transport,
      )),
    ));
    await tester.pumpAndSettle();

    await tester.tap(find.text('设备'));
    await tester.pumpAndSettle();
    final manualPairing = find.text('手动输入配对码');
    await tester.scrollUntilVisible(manualPairing, 120,
        scrollable: find.byType(Scrollable).first);
    await tester.tap(manualPairing);
    await tester.pumpAndSettle();
    await tester.enterText(
        find.descendant(
            of: find.byType(AlertDialog), matching: find.byType(TextField)),
        syntheticPairingPayload());
    await tester.tap(find.text('下一步'));
    await tester.pumpAndSettle();
    await tester.pump(const Duration(milliseconds: 300));
    await tester.pumpAndSettle();
    await tester.tap(find.text('确认配对'));
    await tester.pumpAndSettle();

    await tester.tap(find.text('传输'));
    await tester.pumpAndSettle();
    final draft = find.byKey(const ValueKey('compose'));
    await tester.enterText(draft, 'draft survives locale change');
    await tester.tap(find.byTooltip('语言'));
    await tester.pumpAndSettle();
    await tester.tap(find.text('English').last);
    await tester.pumpAndSettle();

    expect(find.text('PeerCarry'), findsOneWidget);
    expect(find.text('Test computer'), findsOneWidget);
    expect(tester.widget<TextField>(draft).controller!.text,
        'draft survives locale change');
    expect(tester.takeException(), isNull);

    final preferences = await SharedPreferences.getInstance();
    expect(preferences.getString('mobile.language'), AppLanguage.english.name);
  });

  testWidgets('saved language is restored on the next app instance',
      (tester) async {
    SharedPreferences.setMockInitialValues({
      'mobile.language': AppLanguage.english.name,
    });
    await tester.pumpWidget(const PeerCarryApp());
    await tester.pumpAndSettle();
    expect(find.text('PeerCarry'), findsOneWidget);
    expect(find.text('Transfer'), findsOneWidget);
    await tester.tap(find.byTooltip('Language'));
    await tester.pumpAndSettle();
    await tester.tap(find.text('Follow system'));
    await tester.pumpAndSettle();
    final preferences = await SharedPreferences.getInstance();
    expect(preferences.getString('mobile.language'), AppLanguage.system.name);
    expect(find.text('Transfer'), findsOneWidget);
  });

  testWidgets('system changes apply until the user selects a language',
      (tester) async {
    final dispatcher = tester.binding.platformDispatcher;
    addTearDown(dispatcher.clearLocaleTestValue);
    addTearDown(dispatcher.clearLocalesTestValue);
    void setSystemLocale(Locale locale) {
      dispatcher.localeTestValue = locale;
      dispatcher.localesTestValue = [locale];
    }

    SharedPreferences.setMockInitialValues({});
    setSystemLocale(const Locale('zh', 'TW'));
    await tester.pumpWidget(const PeerCarryApp());
    await tester.pumpAndSettle();
    expect(find.text('传输'), findsOneWidget);

    setSystemLocale(const Locale('ja', 'JP'));
    await tester.pumpAndSettle();
    expect(find.text('Transfer'), findsOneWidget);

    await tester.tap(find.byTooltip('Language'));
    await tester.pumpAndSettle();
    await tester.tap(find.text('简体中文'));
    await tester.pumpAndSettle();
    setSystemLocale(const Locale('en', 'US'));
    await tester.pumpAndSettle();
    expect(find.text('传输'), findsOneWidget);

    await tester.tap(find.byTooltip('语言'));
    await tester.pumpAndSettle();
    await tester.tap(find.text('跟随系统'));
    await tester.pumpAndSettle();
    expect(find.text('Transfer'), findsOneWidget);
    expect(tester.takeException(), isNull);
  });

  testWidgets('preference storage failures use safe fallback and fixed notice',
      (tester) async {
    await tester.pumpWidget(const PeerCarryApp(
      preferenceStore: _FailingPreferenceStore(),
    ));
    await tester.pumpAndSettle();
    expect(find.text('PeerCarry'), findsOneWidget);

    await tester.tap(find.byTooltip('Language'));
    await tester.pumpAndSettle();
    await tester.tap(find.text('简体中文'));
    await tester.pumpAndSettle();

    expect(find.text('PeerCarry 随传'), findsOneWidget);
    expect(find.text('语言设置仅在本次运行有效，无法保存。'), findsOneWidget);
    expect(find.textContaining('private details'), findsNothing);
    expect(tester.takeException(), isNull);
  });
}
