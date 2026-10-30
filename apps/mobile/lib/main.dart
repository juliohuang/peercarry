import 'package:file_selector/file_selector.dart';
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_localizations/flutter_localizations.dart';
import 'package:shared_preferences/shared_preferences.dart';

import 'client.dart';
import 'models.dart';
import 'login_assist_page.dart';
import 'app_theme.dart';
import 'connection_client.dart';
import 'login_assist_scanner.dart';
import 'app_localizations.dart';

void main() => runApp(const PeerCarryApp());

abstract interface class LanguagePreferenceStore {
  Future<String?> load();
  Future<bool> save(String languageName);
}

class SharedPreferencesLanguagePreferenceStore
    implements LanguagePreferenceStore {
  const SharedPreferencesLanguagePreferenceStore();

  static const _key = 'mobile.language';

  @override
  Future<String?> load() async =>
      (await SharedPreferences.getInstance()).getString(_key);

  @override
  Future<bool> save(String languageName) async =>
      (await SharedPreferences.getInstance()).setString(_key, languageName);
}

class PeerCarryApp extends StatefulWidget {
  const PeerCarryApp({
    super.key,
    this.pair = PairedConnection.pair,
    this.preferenceStore = const SharedPreferencesLanguagePreferenceStore(),
  });

  final Future<PairedConnection> Function(ConnectionTicket) pair;
  final LanguagePreferenceStore preferenceStore;

  @override
  State<PeerCarryApp> createState() => _PeerCarryAppState();
}

class _PeerCarryAppState extends State<PeerCarryApp> {
  AppLanguage _language = AppLanguage.system;
  bool _languageTouched = false;

  @override
  void initState() {
    super.initState();
    _restoreLanguage();
  }

  Future<void> _restoreLanguage() async {
    try {
      final saved = await widget.preferenceStore.load();
      final selected = AppLanguage.values.where((value) => value.name == saved);
      if (!mounted || selected.isEmpty || _languageTouched) return;
      setState(() => _language = selected.first);
    } catch (_) {
      // Keep the system-language default if local storage cannot be read.
    }
  }

  Future<bool> _selectLanguage(AppLanguage language) async {
    setState(() {
      _languageTouched = true;
      _language = language;
    });
    try {
      return await widget.preferenceStore.save(language.name);
    } catch (_) {
      return false;
    }
  }

  @override
  Widget build(BuildContext context) => MaterialApp(
        title: 'PeerCarry',
        debugShowCheckedModeBanner: false,
        theme: carryTheme(),
        locale: switch (_language) {
          AppLanguage.system => null,
          AppLanguage.simplifiedChinese => const Locale('zh', 'CN'),
          AppLanguage.english => const Locale('en'),
        },
        supportedLocales: AppLocalizations.supportedLocales,
        localizationsDelegates: const [
          AppLocalizations.delegate,
          GlobalMaterialLocalizations.delegate,
          GlobalWidgetsLocalizations.delegate,
          GlobalCupertinoLocalizations.delegate,
        ],
        localeResolutionCallback: (locale, supportedLocales) =>
            AppLocalizations.resolve(locale),
        home: HomePage(pair: widget.pair),
      );

  Future<void> openLanguagePicker(BuildContext context) async {
    final selected = await showModalBottomSheet<AppLanguage>(
      context: context,
      builder: (context) {
        final l10n = AppLocalizations.of(context);
        return SafeArea(
          child: Column(mainAxisSize: MainAxisSize.min, children: [
            Padding(
              padding: const EdgeInsets.fromLTRB(20, 20, 20, 8),
              child: Align(
                alignment: Alignment.centerLeft,
                child: Text(l10n.text('languageTitle'),
                    style: const TextStyle(
                        fontSize: 18, fontWeight: FontWeight.w700)),
              ),
            ),
            for (final language in AppLanguage.values)
              Semantics(
                  checked: language == _language,
                  inMutuallyExclusiveGroup: true,
                  child: ListTile(
                    leading: Icon(
                        language == _language
                            ? Icons.radio_button_checked
                            : Icons.radio_button_unchecked,
                        color: Theme.of(context).colorScheme.primary),
                    title: Text(switch (language) {
                      AppLanguage.system => l10n.text('system'),
                      AppLanguage.simplifiedChinese => l10n.text('chinese'),
                      AppLanguage.english => l10n.text('english'),
                    }),
                    onTap: () => Navigator.pop(context, language),
                  )),
            const SizedBox(height: 12),
          ]),
        );
      },
    );
    if (selected != null &&
        !await _selectLanguage(selected) &&
        context.mounted) {
      final effectiveLocale = switch (_language) {
        AppLanguage.system => AppLocalizations.resolve(
            WidgetsBinding.instance.platformDispatcher.locale),
        AppLanguage.simplifiedChinese => const Locale('zh', 'CN'),
        AppLanguage.english => const Locale('en'),
      };
      ScaffoldMessenger.of(context).showSnackBar(
        SnackBar(
          content: Text(
              AppLocalizations(effectiveLocale).text('languageSaveFailed')),
        ),
      );
    }
  }
}

class HomePage extends StatefulWidget {
  const HomePage({super.key, this.pair = PairedConnection.pair});

  final Future<PairedConnection> Function(ConnectionTicket) pair;

  @override
  State<HomePage> createState() => _HomePageState();
}

class _HomePageState extends State<HomePage> {
  final text = TextEditingController();

  PeerCarryClient? client;
  PairedConnection? paired;
  Hello? hello;
  List<Entry> entries = const [];
  String message = '';
  String? messageKey;
  bool busy = false;
  double? progress;
  int _generation = 0;
  int tab = 0;
  String filter = 'all';
  bool messageIsError = false;

  AppLocalizations get l10n => AppLocalizations.of(context);

  @override
  void dispose() {
    client?.dispose();
    text.dispose();
    super.dispose();
  }

  void noteKey(String key, {bool isError = false}) {
    if (mounted) {
      setState(() {
        message = '';
        messageKey = key;
        messageIsError = isError;
      });
    }
  }

  void disconnect() {
    _generation++;
    client?.dispose();
    client = null;
    paired = null;
    hello = null;
    entries = const [];
    message = '';
    messageKey = null;
    if (mounted) setState(() {});
  }

  Future<void> refreshEntries(PeerCarryClient current) async {
    final refreshed = await current.entries();
    if (mounted && identical(client, current)) {
      setState(() => entries = refreshed);
    }
  }

  Future<void> scanComputer() async {
    final value = await Navigator.of(context).push<String>(MaterialPageRoute(
        builder: (_) => const LoginAssistScanner(forTransfer: true)));
    if (!mounted || value == null) return;
    await pairComputer(value);
  }

  Future<void> pastePairing() async {
    final controller = TextEditingController();
    final value = await showDialog<String>(
        context: context,
        builder: (context) => AlertDialog(
                title: Text(l10n.text('enterPairingCode')),
                content: TextField(
                    controller: controller,
                    maxLines: 4,
                    autocorrect: false,
                    enableSuggestions: false,
                    enableIMEPersonalizedLearning: false,
                    decoration:
                        InputDecoration(hintText: l10n.text('pastePairing'))),
                actions: [
                  TextButton(
                      onPressed: () => Navigator.pop(context),
                      child: Text(l10n.text('cancel'))),
                  FilledButton(
                      onPressed: () => Navigator.pop(context, controller.text),
                      child: Text(l10n.text('next')))
                ]));
    // The dialog's text field is disposed after its closing animation.
    await Future<void>.delayed(const Duration(milliseconds: 250));
    controller.dispose();
    if (!mounted || value == null) return;
    await pairComputer(value);
  }

  Future<void> pairComputer(String value) async {
    ConnectionTicket ticket;
    try {
      ticket = ConnectionTicket.parse(value);
    } catch (_) {
      return noteKey('pairFailure', isError: true);
    }
    final confirmed = await showDialog<bool>(
        context: context,
        builder: (context) => AlertDialog(
                title:
                    Text(l10n.format('connectQuestion', {'host': ticket.host})),
                content: Text(l10n.text('pairingPrivacy')),
                actions: [
                  TextButton(
                      onPressed: () => Navigator.pop(context, false),
                      child: Text(l10n.text('cancel'))),
                  FilledButton(
                      onPressed: () => Navigator.pop(context, true),
                      child: Text(l10n.text('confirmPairing')))
                ]));
    if (!mounted || confirmed != true) return;
    disconnect();
    final generation = ++_generation;
    setState(() => busy = true);
    PairedConnection? connection;
    PeerCarryClient? candidate;
    try {
      connection = await widget.pair(ticket);
      candidate = connection.createClient();
      final connectedHello = await candidate.hello();
      final connectedEntries = await candidate.entries();
      if (!mounted || generation != _generation) {
        candidate.dispose();
        return;
      }
      paired = connection;
      client = candidate;
      hello = connectedHello;
      entries = connectedEntries;
      connection.route.addListener(() {
        if (mounted && identical(paired, connection)) setState(() {});
      });
      message = '';
      messageKey = 'pairSuccess';
      messageIsError = false;
      tab = 0;
    } catch (e) {
      candidate?.dispose();
      connection?.close();
      if (mounted && generation == _generation) {
        noteKey('pairFailure', isError: true);
      }
    } finally {
      if (mounted && generation == _generation) setState(() => busy = false);
    }
  }

  String get connectionLabel => switch (paired?.route.value) {
        ConnectionRoute.lan => l10n.text('lanConnection'),
        ConnectionRoute.relay => l10n.text('relayConnection'),
        null => l10n.text('connectionLost'),
      };

  Future<void> send() async {
    final current = client;
    final value = text.text;
    if (current == null) return noteKey('pleaseConnect');
    if (value.trim().isEmpty) return noteKey('enterText');
    if (mounted) setState(() => busy = true);
    try {
      await current.sendText(value);
      if (!mounted || !identical(client, current)) return;
      text.clear();
      noteKey('textSent');
      try {
        await refreshEntries(current);
      } catch (_) {
        noteKey('textSentHistoryFailed');
      }
    } catch (error) {
      noteKey('sendFailed', isError: true);
    } finally {
      if (mounted) setState(() => busy = false);
    }
  }

  Future<void> chooseFile() async {
    final current = client;
    if (current == null) return noteKey('pleaseConnect');
    if (!(hello?.capabilities.contains('mobile-upload-v1') ?? false)) {
      return noteKey('uploadCapability', isError: true);
    }
    if (mounted) {
      setState(() {
        busy = true;
        progress = 0;
      });
    }
    try {
      final file = await openFile();
      if (file == null || !mounted) return;
      await current.upload(file, onProgress: (sent, total) {
        if (mounted && identical(client, current)) {
          setState(() => progress = total == 0 ? 0 : sent / total);
        }
      });
      if (!mounted || !identical(client, current)) return;
      noteKey('fileUploaded');
      try {
        await refreshEntries(current);
      } catch (_) {
        noteKey('fileUploadedHistoryFailed');
      }
    } catch (error) {
      noteKey('uploadFailed', isError: true);
    } finally {
      if (mounted) {
        setState(() {
          busy = false;
          progress = null;
        });
      }
    }
  }

  Future<void> reload() async {
    final current = client;
    if (current == null || busy) return;
    setState(() => busy = true);
    try {
      await refreshEntries(current);
      if (mounted && identical(client, current)) noteKey('historyRefreshed');
    } catch (_) {
      noteKey('refreshFailed', isError: true);
    } finally {
      if (mounted) setState(() => busy = false);
    }
  }

  void openLoginAssist() => Navigator.of(context)
      .push(MaterialPageRoute<void>(builder: (_) => const LoginAssistPage()));

  @override
  Widget build(BuildContext context) => Scaffold(
        appBar: AppBar(
          title: Row(mainAxisSize: MainAxisSize.min, children: [
            const Icon(Icons.link_rounded, color: CarryColors.forest, size: 30),
            const SizedBox(width: 8),
            Flexible(
              child: Text(l10n.text('appTitle'),
                  style: const TextStyle(
                      fontSize: 19, fontWeight: FontWeight.w700)),
            ),
          ]),
          actions: [
            IconButton(
              icon: const Icon(Icons.language_rounded),
              tooltip: l10n.text('language'),
              onPressed: () => context
                  .findAncestorStateOfType<_PeerCarryAppState>()
                  ?.openLanguagePicker(context),
            ),
            IconButton(
                icon: const Icon(Icons.phonelink_lock_outlined),
                tooltip: l10n.text('loginAssist'),
                onPressed: openLoginAssist),
            const SizedBox(width: 8),
          ],
        ),
        body: SafeArea(
          top: false,
          bottom: false,
          child: Align(
            alignment: Alignment.topCenter,
            child: ConstrainedBox(
              constraints: const BoxConstraints(maxWidth: 640),
              child: ListView(
                key: ValueKey(tab),
                keyboardDismissBehavior:
                    ScrollViewKeyboardDismissBehavior.onDrag,
                padding: const EdgeInsets.fromLTRB(20, 12, 20, 28),
                children: tab == 0 ? transferContent() : deviceContent(),
              ),
            ),
          ),
        ),
        bottomNavigationBar: DecoratedBox(
          decoration: const BoxDecoration(
              border: Border(top: BorderSide(color: CarryColors.border))),
          child: NavigationBar(
            selectedIndex: tab,
            onDestinationSelected: (value) {
              FocusManager.instance.primaryFocus?.unfocus();
              setState(() => tab = value);
            },
            destinations: [
              NavigationDestination(
                  icon: const Icon(Icons.send_outlined),
                  selectedIcon: const Icon(Icons.send_rounded),
                  label: l10n.text('transfer')),
              NavigationDestination(
                  icon: const Icon(Icons.computer_outlined),
                  selectedIcon: const Icon(Icons.computer_rounded),
                  label: l10n.text('devices')),
            ],
          ),
        ),
      );

  Widget heading(String title, String subtitle) => Padding(
        padding: const EdgeInsets.only(bottom: 22),
        child: Column(crossAxisAlignment: CrossAxisAlignment.start, children: [
          Text(title,
              style: const TextStyle(
                  fontSize: 27, height: 1.3, fontWeight: FontWeight.w700)),
          const SizedBox(height: 8),
          Text(subtitle,
              style: const TextStyle(color: CarryColors.muted, height: 1.5)),
        ]),
      );

  List<Widget> feedback() => [
        if (busy)
          Padding(
            padding: const EdgeInsets.only(bottom: 16),
            child:
                Column(crossAxisAlignment: CrossAxisAlignment.start, children: [
              LinearProgressIndicator(
                  value: progress,
                  borderRadius: BorderRadius.circular(8),
                  minHeight: 5),
              const SizedBox(height: 8),
              Text(
                  progress != null
                      ? l10n.format(
                          'uploading', {'percent': (progress! * 100).round()})
                      : l10n.text('processing'),
                  style:
                      const TextStyle(fontSize: 12, color: CarryColors.muted)),
            ]),
          ),
        if (message.isNotEmpty || messageKey != null)
          Padding(
            padding: const EdgeInsets.only(bottom: 16),
            child: CarryNotice(
                text: messageKey == null ? message : l10n.text(messageKey!),
                isError: messageIsError,
                icon: messageIsError
                    ? Icons.error_outline
                    : Icons.check_circle_outline),
          ),
      ];

  List<Widget> transferContent() {
    final connected = client != null;
    final canUpload = hello?.capabilities.contains('mobile-upload-v1') ?? false;
    final visible =
        entries.where((entry) => filter == 'all' || entry.kind == filter);
    return [
      heading(l10n.text('heading'), l10n.text('headingSubtitle')),
      Material(
        color: CarryColors.forest,
        borderRadius: BorderRadius.circular(20),
        clipBehavior: Clip.antiAlias,
        child: InkWell(
          onTap: () => setState(() => tab = 1),
          child: Padding(
            padding: const EdgeInsets.all(20),
            child: Row(children: [
              const Icon(Icons.desktop_windows_outlined,
                  size: 40, color: Colors.white),
              const SizedBox(width: 18),
              Expanded(
                child: Column(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      Text(l10n.text('currentConnection'),
                          style: const TextStyle(
                              color: Color(0xFFCADFD6), fontSize: 12)),
                      const SizedBox(height: 6),
                      Text(
                          connected
                              ? hello!.host
                              : l10n.text('connectComputer'),
                          style: const TextStyle(
                              color: Colors.white,
                              fontSize: 18,
                              fontWeight: FontWeight.w600)),
                      const SizedBox(height: 8),
                      Row(children: [
                        Icon(Icons.circle,
                            size: 7,
                            color: connected
                                ? const Color(0xFF9AE4B2)
                                : const Color(0xFFCADFD6)),
                        const SizedBox(width: 6),
                        Flexible(
                            child: Text(
                                connected
                                    ? connectionLabel
                                    : l10n.text('notConnected'),
                                style: const TextStyle(
                                    color: Color(0xFFCADFD6), fontSize: 12))),
                      ]),
                    ]),
              ),
              const SizedBox(width: 8),
              const Icon(Icons.chevron_right, color: Colors.white),
            ]),
          ),
        ),
      ),
      const SizedBox(height: 18),
      ...feedback(),
      CarryCard(
        child: Column(crossAxisAlignment: CrossAxisAlignment.start, children: [
          Text(l10n.text('sendContent'),
              style:
                  const TextStyle(fontSize: 17, fontWeight: FontWeight.w700)),
          const SizedBox(height: 14),
          TextField(
            key: const ValueKey('compose'),
            controller: text,
            enabled: connected && !busy,
            minLines: 3,
            maxLines: 6,
            maxLength: 65536,
            onChanged: (_) => setState(() {}),
            decoration: InputDecoration(
              hintText: connected
                  ? l10n.text('composeConnected')
                  : l10n.text('composeDisconnected'),
              counterText: '',
            ),
          ),
          const SizedBox(height: 14),
          LayoutBuilder(builder: (context, constraints) {
            final buttons = [
              OutlinedButton.icon(
                  onPressed:
                      connected && !busy && canUpload ? chooseFile : null,
                  icon: const Icon(Icons.attach_file_rounded, size: 19),
                  label: Text(l10n.text('chooseFile'))),
              FilledButton.icon(
                  onPressed: connected && !busy && text.text.trim().isNotEmpty
                      ? send
                      : null,
                  icon: const Icon(Icons.send_outlined, size: 18),
                  label: Text(l10n.text('sendText'))),
            ];
            if (constraints.maxWidth < 300 ||
                MediaQuery.textScalerOf(context).scale(14) > 19) {
              return Column(
                  crossAxisAlignment: CrossAxisAlignment.stretch,
                  children: [
                    buttons[0],
                    const SizedBox(height: 10),
                    buttons[1],
                  ]);
            }
            return Row(children: [
              Expanded(child: buttons[0]),
              const SizedBox(width: 10),
              Expanded(child: buttons[1]),
            ]);
          }),
          if (connected && !canUpload)
            Padding(
                padding: const EdgeInsets.only(top: 12),
                child: Text(l10n.text('uploadUnavailable'),
                    style: const TextStyle(
                        color: CarryColors.muted, fontSize: 12))),
        ]),
      ),
      const SizedBox(height: 26),
      Row(children: [
        Expanded(
            child: Text(l10n.text('sharedHistory'),
                style: const TextStyle(
                    fontSize: 20, fontWeight: FontWeight.w700))),
        IconButton(
            onPressed: connected && !busy ? reload : null,
            tooltip: l10n.text('refreshHistory'),
            icon: const Icon(Icons.refresh_rounded)),
      ]),
      Text(l10n.text('clipboardManual'),
          style: const TextStyle(fontSize: 12, color: CarryColors.muted)),
      const SizedBox(height: 12),
      Wrap(spacing: 8, runSpacing: 8, children: [
        for (final item in {
          'all': l10n.text('all'),
          'text': l10n.text('text'),
          'image': l10n.text('image'),
          'files': l10n.text('files')
        }.entries)
          ChoiceChip(
              label: Text(item.value),
              selected: filter == item.key,
              showCheckmark: false,
              onSelected: (_) => setState(() => filter = item.key)),
      ]),
      const SizedBox(height: 14),
      if (visible.isEmpty)
        CarryCard(
          child: Column(children: [
            const SizedBox(height: 10),
            const Icon(Icons.inbox_outlined,
                size: 40, color: CarryColors.muted),
            const SizedBox(height: 12),
            Text(
                !connected
                    ? l10n.text('sharedContent')
                    : entries.isEmpty
                        ? l10n.text('noSharedContent')
                        : l10n.text('noItemsOfType'),
                style: const TextStyle(fontWeight: FontWeight.w600)),
            const SizedBox(height: 6),
            Text(
                !connected
                    ? l10n.text('connectThenHistory')
                    : l10n.text('captureThenRefresh'),
                textAlign: TextAlign.center,
                style: const TextStyle(
                    color: CarryColors.muted, fontSize: 12, height: 1.5)),
            const SizedBox(height: 10),
          ]),
        ),
      for (final entry in visible) historyCard(entry),
    ];
  }

  Widget historyCard(Entry entry) {
    final kind = switch (entry.kind) {
      'text' => l10n.text('text'),
      'image' => l10n.text('image'),
      'files' => l10n.text('files'),
      _ => l10n.text('text'),
    };
    final icon = switch (entry.kind) {
      'text' => Icons.notes_rounded,
      'image' => Icons.image_outlined,
      _ => Icons.insert_drive_file_outlined,
    };
    return Padding(
      padding: const EdgeInsets.only(bottom: 10),
      child: CarryCard(
        padding: const EdgeInsets.all(16),
        child: Row(crossAxisAlignment: CrossAxisAlignment.start, children: [
          Container(
              padding: const EdgeInsets.all(10),
              decoration: BoxDecoration(
                  color: CarryColors.mint,
                  borderRadius: BorderRadius.circular(12)),
              child: Icon(icon, color: CarryColors.forest, size: 22)),
          const SizedBox(width: 12),
          Expanded(
              child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                Text(
                    entry.display.isEmpty
                        ? '$kind ${l10n.text('sharedContent')}'
                        : entry.display,
                    maxLines: 3,
                    overflow: TextOverflow.ellipsis,
                    style: const TextStyle(fontSize: 14, height: 1.5)),
                const SizedBox(height: 5),
                Text(
                    entry.isText ? kind : '$kind · ${l10n.text('previewOnly')}',
                    style: const TextStyle(
                        color: CarryColors.muted, fontSize: 12)),
              ])),
          if (entry.isText)
            IconButton(
              icon: const Icon(Icons.copy_outlined, size: 20),
              tooltip: l10n.text('copyClipboard'),
              onPressed: () async {
                try {
                  await Clipboard.setData(ClipboardData(text: entry.text!));
                  noteKey('copySuccess');
                } catch (_) {
                  noteKey('copyFailed', isError: true);
                }
              },
            ),
        ]),
      ),
    );
  }

  List<Widget> deviceContent() => [
        heading(l10n.text('connectHeading'), l10n.text('connectSubtitle')),
        const Padding(
          padding: EdgeInsets.only(bottom: 24),
          child: _DeviceIllustration(),
        ),
        ...feedback(),
        CarryCard(
            child: Column(
                crossAxisAlignment: CrossAxisAlignment.stretch,
                children: [
              Text(l10n.text('addComputer'),
                  style: const TextStyle(
                      fontSize: 20, fontWeight: FontWeight.w700)),
              const SizedBox(height: 10),
              Text(l10n.text('pairingHelp'),
                  style:
                      const TextStyle(color: CarryColors.muted, height: 1.6)),
              const SizedBox(height: 20),
              FilledButton.icon(
                  onPressed: busy ? null : scanComputer,
                  icon: const Icon(Icons.qr_code_scanner_rounded),
                  label: Text(l10n.text('scanConnect'))),
              TextButton(
                  onPressed: busy ? null : pastePairing,
                  child: Text(l10n.text('manualPairing'))),
              if (paired != null) ...[
                const Divider(height: 28),
                Text(hello!.host,
                    style: const TextStyle(fontWeight: FontWeight.w600)),
                const SizedBox(height: 6),
                Text(connectionLabel,
                    style: const TextStyle(color: CarryColors.forest)),
                TextButton(
                    onPressed: busy
                        ? null
                        : () {
                            disconnect();
                            noteKey('connectionLost');
                          },
                    child: Text(l10n.text('disconnect'))),
              ],
            ])),
        const SizedBox(height: 16),
        CarryNotice(text: l10n.text('sessionPrivacyNotice')),
        const SizedBox(height: 18),
        CarryCard(
          padding: EdgeInsets.zero,
          child: ListTile(
            contentPadding:
                const EdgeInsets.symmetric(horizontal: 20, vertical: 10),
            leading: const Icon(Icons.qr_code_scanner_rounded,
                color: CarryColors.forest, size: 28),
            title: Text(l10n.text('loginAssist'),
                style: const TextStyle(fontWeight: FontWeight.w600)),
            subtitle: Padding(
                padding: const EdgeInsets.only(top: 4),
                child: Text(l10n.text('inDevelopmentSynthetic'),
                    style: const TextStyle(
                        fontSize: 12, color: CarryColors.muted))),
            trailing: const Icon(Icons.chevron_right),
            onTap: openLoginAssist,
          ),
        ),
      ];
}

class _DeviceIllustration extends StatelessWidget {
  const _DeviceIllustration();
  @override
  Widget build(BuildContext context) => Center(
        child: Container(
          width: 240,
          padding: const EdgeInsets.symmetric(vertical: 22),
          decoration: BoxDecoration(
              color: CarryColors.mint, borderRadius: BorderRadius.circular(60)),
          child:
              const Row(mainAxisAlignment: MainAxisAlignment.center, children: [
            Icon(Icons.smartphone_rounded, size: 49, color: CarryColors.forest),
            Padding(
                padding: EdgeInsets.symmetric(horizontal: 12),
                child: Icon(Icons.more_horiz_rounded,
                    size: 28, color: CarryColors.forest)),
            Icon(Icons.laptop_mac_rounded, size: 64, color: CarryColors.forest),
          ]),
        ),
      );
}
