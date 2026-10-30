import 'package:flutter/material.dart';
import 'package:flutter_localizations/flutter_localizations.dart';
import 'package:peercarry_mobile/app_localizations.dart';

Widget localizedTestApp({
  required ThemeData theme,
  required Widget home,
  TransitionBuilder? builder,
  Locale locale = const Locale('zh', 'CN'),
}) =>
    MaterialApp(
      theme: theme,
      builder: builder,
      locale: locale,
      supportedLocales: AppLocalizations.supportedLocales,
      localizationsDelegates: const [
        AppLocalizations.delegate,
        GlobalMaterialLocalizations.delegate,
        GlobalWidgetsLocalizations.delegate,
        GlobalCupertinoLocalizations.delegate,
      ],
      home: home,
    );
