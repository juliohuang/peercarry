// Render synthetic UI previews with the real Flutter widgets.
// flutter test --no-pub tool/render_ui_test.dart
// Set PEERCARRY_UI_OUTPUT and PEERCARRY_UI_FONT (a local Chinese font file).
import 'dart:convert';
import 'dart:io';
import 'dart:ui' as ui;

import 'package:flutter/material.dart';
import 'package:flutter/rendering.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:http/testing.dart';
import 'package:peercarry_mobile/app_theme.dart';
import 'package:peercarry_mobile/client.dart';
import 'package:peercarry_mobile/login_assist_page.dart';
import 'package:peercarry_mobile/main.dart';
import '../test/support/paired_fixture.dart';

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();
  final output = Platform.environment['PEERCARRY_UI_OUTPUT'];
  final font = Platform.environment['PEERCARRY_UI_FONT'];
  setUpAll(() async {
    if (font != null) {
      final bytes = ByteData.sublistView(await File(font).readAsBytes());
      // Widget tests default to Ahem; replace it for a legible visual preview.
      for (final family in ['PreviewSans', 'Roboto', 'Ahem']) {
        await (FontLoader(family)..addFont(Future.value(bytes))).load();
      }
    }
    await (FontLoader('MaterialIcons')
          ..addFont(rootBundle.load('fonts/MaterialIcons-Regular.otf')))
        .load();
  });

  testWidgets('render app screens with synthetic content', (tester) async {
    if (output == null) {
      fail('Set PEERCARRY_UI_OUTPUT to an output directory.');
    }
    await tester.binding.setSurfaceSize(const Size(412, 915));
    addTearDown(() => tester.binding.setSurfaceSize(null));
    final boundaryKey = GlobalKey();
    final theme = carryTheme();
    Widget app(Widget child) => RepaintBoundary(
          key: boundaryKey,
          child: MaterialApp(
            debugShowCheckedModeBanner: false,
            theme: font == null
                ? theme
                : theme.copyWith(
                    textTheme: theme.textTheme.apply(fontFamily: 'PreviewSans'),
                    primaryTextTheme:
                        theme.primaryTextTheme.apply(fontFamily: 'PreviewSans'),
                    filledButtonTheme: FilledButtonThemeData(
                      style: theme.filledButtonTheme.style!.copyWith(
                        textStyle: const WidgetStatePropertyAll(TextStyle(
                          fontFamily: 'PreviewSans',
                          fontSize: 15,
                          fontWeight: FontWeight.w600,
                        )),
                      ),
                    ),
                    chipTheme: theme.chipTheme.copyWith(
                      labelStyle: theme.chipTheme.labelStyle!
                          .copyWith(fontFamily: 'PreviewSans'),
                    ),
                  ),
            home: child,
          ),
        );

    Future<void> capture(String name) async {
      await tester.pumpAndSettle();
      expect(tester.takeException(), isNull);
      final boundary = boundaryKey.currentContext!.findRenderObject()!
          as RenderRepaintBoundary;
      await tester.runAsync(() async {
        final image = await boundary.toImage(pixelRatio: 2);
        final data = await image.toByteData(format: ui.ImageByteFormat.png);
        await Directory(output).create(recursive: true);
        await File('$output/$name.png')
            .writeAsBytes(data!.buffer.asUint8List());
        image.dispose();
      });
    }

    await tester.pumpWidget(app(const HomePage()));
    await capture('mobile-ui-home-empty');
    await tester.tap(find.text('设备'));
    await capture('mobile-ui-devices');
    await tester.pumpWidget(const SizedBox());
    await tester.pumpWidget(app(const LoginAssistPage()));
    await capture('mobile-ui-login');
    await tester.pumpWidget(const SizedBox());

    final transport = MockClient((request) async {
      if (request.url.path == '/v1/hello') {
        return http.Response(
            jsonEncode({
              'protocol': 2,
              'host': '工作电脑',
              'version': '0.3.1',
              'capabilities': ['mobile-upload-v1'],
            }),
            200,
            headers: {'content-type': 'application/json; charset=utf-8'});
      }
      return http.Response(
          jsonEncode([
            {
              'id': 'preview-text',
              'kind': 'text',
              'text': '周末出行清单\n帐篷、睡袋、防潮垫、烧烤炉、食材…'
            },
            {'id': 'preview-file', 'kind': 'files', 'preview': '设计参考.pdf'},
          ]),
          200,
          headers: {'content-type': 'application/json; charset=utf-8'});
    });
    await tester.pumpWidget(app(HomePage(
      pair: (_) async => FakePairedConnection(PeerCarryClient(
          baseUrl: 'https://paired.peercarry.invalid',
          deviceId: 'synthetic-phone',
          transport: transport)),
    )));
    await showDevices(tester);
    await enterPairing(tester);
    await confirmPairing(tester);
    await tester.enterText(
        find.byKey(const ValueKey('compose')), '这段灵感，接着在电脑上写。');
    tester.testTextInput.hide();
    FocusManager.instance.primaryFocus?.unfocus();
    await capture('mobile-ui-home-connected');
    await tester.pumpWidget(const SizedBox());
  });
}
