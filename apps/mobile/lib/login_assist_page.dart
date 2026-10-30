import 'dart:async';
import 'dart:convert';
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'login_assist_client.dart';
import 'login_assist_scanner.dart';
import 'app_theme.dart';
import 'app_localizations.dart';

class LoginAssistPage extends StatefulWidget {
  const LoginAssistPage({super.key});
  @override
  State<LoginAssistPage> createState() => _LoginAssistPageState();
}

class _LoginAssistPageState extends State<LoginAssistPage>
    with WidgetsBindingObserver {
  static const privacy = MethodChannel('dev.peercarry/privacy');
  final ticket = TextEditingController();
  final input = TextEditingController();
  LoginAssistClient? client;
  Map<String, dynamic>? state;
  Map<String, dynamic>? frame;
  Uint8List? pixels;
  String? field;
  String message = '';
  String? messageKey;
  bool busy = false;
  bool clicks = false;
  int generation = 0;
  Timer? timer;
  String get phase => state?['status']?['phase'] as String? ?? '';
  int get epoch => state?['status']?['epoch'] as int? ?? 0;
  AppLocalizations get l10n => AppLocalizations.of(context);

  @override
  void initState() {
    super.initState();
    WidgetsBinding.instance.addObserver(this);
    unawaited(privacy.invokeMethod<void>('enable').catchError((_) {}));
    timer = Timer.periodic(const Duration(seconds: 2), (_) {
      if (client != null && !busy) unawaited(run((c) => c.command('state')));
    });
  }

  @override
  void didChangeAppLifecycleState(AppLifecycleState lifecycle) {
    if (lifecycle != AppLifecycleState.resumed) stop();
  }

  void clearPrivate() {
    input.clear();
    if (pixels != null) unawaited(MemoryImage(pixels!).evict());
    pixels?.fillRange(0, pixels!.length, 0);
    pixels = null;
    frame = null;
    clicks = false;
  }

  void stop() {
    generation++;
    final old = client;
    client = null;
    clearPrivate();
    ticket.clear();
    state = null;
    field = null;
    busy = false;
    if (old != null) unawaited(old.disconnect());
    if (mounted) {
      setState(() {
        message = '';
        messageKey = 'connectionEnded';
      });
    }
  }

  @override
  void dispose() {
    WidgetsBinding.instance.removeObserver(this);
    timer?.cancel();
    generation++;
    final old = client;
    if (old != null) unawaited(old.disconnect());
    clearPrivate();
    ticket.dispose();
    input.dispose();
    unawaited(privacy.invokeMethod<void>('disable').catchError((_) {}));
    super.dispose();
  }

  void apply(Map<String, dynamic> data) {
    final oldEpoch = epoch;
    state = data;
    final fields = (data['fields'] as List).cast<Map<String, dynamic>>();
    if (!fields.any((f) => f['id'] == field)) {
      field = fields.isEmpty ? null : fields.first['id'] as String;
    }
    if (oldEpoch != epoch || phase != 'Controlling') clearPrivate();
    if (data['data']?['png'] is String) {
      if (pixels != null) unawaited(MemoryImage(pixels!).evict());
      pixels?.fillRange(0, pixels!.length, 0);
      pixels = base64Decode(data['data']['png'] as String);
      frame = data['data'] as Map<String, dynamic>;
    }
  }

  Future<void> connect() async {
    if (busy) return;
    final current = ++generation;
    setState(() {
      busy = true;
      message = '';
      messageKey = null;
    });
    LoginAssistClient? candidate;
    try {
      final parsed = LoginTicket.parse(ticket.text);
      ticket.clear();
      candidate = LoginAssistClient(parsed);
      await candidate.pair(parsed);
      final result = await candidate.command('state');
      if (!mounted || current != generation) {
        candidate.close();
        return;
      }
      client = candidate;
      apply(result);
    } catch (e) {
      candidate?.close();
      if (mounted && current == generation) {
        message = '';
        messageKey = 'loginPairFailed';
      }
    } finally {
      if (mounted && current == generation) setState(() => busy = false);
    }
  }

  Future<void> run(
      Future<Map<String, dynamic>> Function(LoginAssistClient) action) async {
    final c = client;
    if (c == null || busy) return;
    final current = generation;
    setState(() => busy = true);
    try {
      final result = await action(c);
      if (!mounted || current != generation) return;
      setState(() {
        apply(result);
        message = '';
        messageKey = null;
      });
    } catch (e) {
      if (!mounted || current != generation) return;
      clearPrivate();
      final code = e is LoginAssistException ? e.code : '';
      if (['UNAUTHORIZED', 'CONNECTION_FAILED', 'DISCONNECTED']
          .contains(code)) {
        stop();
      }
      message = '';
      messageKey =
          ['UNAUTHORIZED', 'CONNECTION_FAILED', 'DISCONNECTED'].contains(code)
              ? 'loginSessionFailed'
              : 'loginActionFailed';
    } finally {
      if (mounted && current == generation) setState(() => busy = false);
    }
  }

  Future<void> send() async {
    if (field == null || input.text.isEmpty || busy) return;
    final value = input.text;
    input.clear();
    await run((c) => c.fill(epoch, field!, value));
  }

  Future<void> handBack() async {
    final confirmed = await showDialog<bool>(
        context: context,
        builder: (context) => AlertDialog(
                title: Text(l10n.text('loginConfirmTitle')),
                content: Text(l10n.text('loginRevokeNotice')),
                actions: [
                  TextButton(
                      onPressed: () => Navigator.pop(context, false),
                      child: Text(l10n.text('continueAction'))),
                  FilledButton(
                      onPressed: () => Navigator.pop(context, true),
                      child: Text(l10n.text('returnComputer')))
                ]));
    if (confirmed == true && mounted) {
      await run((c) => c.command('hand_back', {'epoch': epoch}));
    }
  }

  String phaseLabel() => switch (phase) {
        'AwaitingUser' => l10n.text('awaitingTarget'),
        'Controlling' => l10n.text('phoneControl'),
        'Suspended' => l10n.text('targetChanged'),
        'Expired' => l10n.text('authorizationExpired'),
        'Completed' => l10n.text('returnedToComputer'),
        'Cancelled' => l10n.text('cancelled'),
        'ResumeFailed' => l10n.text('executorResumeFailed'),
        _ => l10n.text('connectionStateNone'),
      };

  @override
  Widget build(BuildContext context) {
    final fields =
        (state?['fields'] as List? ?? []).cast<Map<String, dynamic>>();
    return Scaffold(
      backgroundColor: CarryColors.background,
      appBar: AppBar(title: Text(l10n.text('loginPageTitle')), actions: [
        IconButton(
            onPressed: client == null ? null : stop,
            icon: const Icon(Icons.link_off),
            tooltip: l10n.text('disconnectRevoke')),
      ]),
      body: SafeArea(
          child: Center(
              child: ConstrainedBox(
                  constraints: const BoxConstraints(maxWidth: 640),
                  child: ListView(
                      padding: const EdgeInsets.fromLTRB(20, 12, 20, 32),
                      children: [
                        if (client == null) ...[
                          const _PairingIllustration(),
                          const SizedBox(height: 16),
                          Text(l10n.text('continueLogin'),
                              style: const TextStyle(
                                  fontSize: 24,
                                  fontWeight: FontWeight.w700,
                                  color: CarryColors.ink)),
                          const SizedBox(height: 8),
                          Text(l10n.text('scanComputerCode'),
                              style: const TextStyle(
                                  color: CarryColors.muted, height: 1.45)),
                          const SizedBox(height: 20),
                          ConstrainedBox(
                            constraints: const BoxConstraints(minHeight: 54),
                            child: FilledButton.icon(
                                onPressed: busy
                                    ? null
                                    : () async {
                                        final value = await Navigator.of(
                                                context)
                                            .push<String>(MaterialPageRoute(
                                                builder: (_) =>
                                                    const LoginAssistScanner()));
                                        if (value != null && mounted) {
                                          ticket.text = value;
                                          await connect();
                                        }
                                      },
                                icon: const Icon(Icons.qr_code_scanner),
                                label: Text(l10n.text('scanComputerPairing'))),
                          ),
                          const SizedBox(height: 16),
                          CarryCard(
                            child: Column(
                                crossAxisAlignment: CrossAxisAlignment.start,
                                children: [
                                  Text(l10n.text('manualTakeover'),
                                      style: const TextStyle(
                                          fontSize: 18,
                                          fontWeight: FontWeight.w700,
                                          color: CarryColors.ink)),
                                  const SizedBox(height: 6),
                                  Text(l10n.text('pasteOneTime'),
                                      style: const TextStyle(
                                          color: CarryColors.muted)),
                                  const SizedBox(height: 14),
                                  TextField(
                                      controller: ticket,
                                      obscureText: true,
                                      autocorrect: false,
                                      enableSuggestions: false,
                                      enableIMEPersonalizedLearning: false,
                                      decoration: InputDecoration(
                                          labelText:
                                              l10n.text('computerOneTime'))),
                                  const SizedBox(height: 12),
                                  SizedBox(
                                      width: double.infinity,
                                      child: OutlinedButton(
                                          onPressed: busy ? null : connect,
                                          child:
                                              Text(l10n.text('pairComputer')))),
                                ]),
                          ),
                          const SizedBox(height: 12),
                          CarryNotice(text: l10n.text('loginPrototypeNotice')),
                        ] else ...[
                          CarryCard(
                            child: Column(
                                crossAxisAlignment: CrossAxisAlignment.start,
                                children: [
                                  Text(l10n.text('currentLoginTarget'),
                                      style: const TextStyle(
                                          fontSize: 18,
                                          fontWeight: FontWeight.w700,
                                          color: CarryColors.ink)),
                                  const SizedBox(height: 8),
                                  SelectableText(
                                      state?['target']?['origin'] as String? ??
                                          '',
                                      style: const TextStyle(
                                          fontSize: 16,
                                          color: CarryColors.ink)),
                                  const SizedBox(height: 10),
                                  _StatusPill(label: phaseLabel()),
                                  const SizedBox(height: 8),
                                  Text(l10n.text('manualNoAi'),
                                      style: const TextStyle(
                                          color: CarryColors.muted)),
                                  if (phase == 'AwaitingUser') ...[
                                    const SizedBox(height: 16),
                                    SizedBox(
                                        width: double.infinity,
                                        child: FilledButton(
                                            onPressed: busy
                                                ? null
                                                : () => run((c) => c.command(
                                                    'approve',
                                                    {'epoch': epoch})),
                                            child: Text(
                                                l10n.text('approveWebsite')))),
                                  ],
                                  if (phase == 'Suspended') ...[
                                    const SizedBox(height: 12),
                                    SizedBox(
                                        width: double.infinity,
                                        child: FilledButton(
                                            onPressed: busy
                                                ? null
                                                : () => run((c) =>
                                                    c.command('refresh')),
                                            child: Text(
                                                l10n.text('reloadTarget')))),
                                    const SizedBox(height: 8),
                                    SizedBox(
                                        width: double.infinity,
                                        child: OutlinedButton(
                                            onPressed: busy ? null : handBack,
                                            child: Text(
                                                l10n.text('loginDoneReturn')))),
                                  ],
                                ]),
                          ),
                          if (phase == 'Controlling') ...[
                            const SizedBox(height: 16),
                            CarryCard(
                              child: Column(
                                  crossAxisAlignment: CrossAxisAlignment.start,
                                  children: [
                                    _SectionTitle(
                                        icon: Icons.pan_tool_outlined,
                                        title: l10n.text('screenOperation')),
                                    Row(children: [
                                      IconButton(
                                          onPressed: busy
                                              ? null
                                              : () => run((c) => c.command(
                                                  'frame', {'epoch': epoch})),
                                          icon: const Icon(Icons.refresh),
                                          tooltip:
                                              l10n.text('refreshViewport')),
                                      IconButton(
                                          onPressed: busy
                                              ? null
                                              : () async {
                                                  final confirmed = await showDialog<
                                                          bool>(
                                                      context: context,
                                                      builder: (context) =>
                                                          AlertDialog(
                                                              title: Text(l10n.text(
                                                                  'refreshPageQuestion')),
                                                              content: Text(
                                                                  l10n.text(
                                                                      'fullPageNotice')),
                                                              actions: [
                                                                TextButton(
                                                                    onPressed: () =>
                                                                        Navigator.pop(
                                                                            context,
                                                                            false),
                                                                    child: Text(
                                                                        l10n.text(
                                                                            'cancel'))),
                                                                FilledButton(
                                                                    onPressed: () =>
                                                                        Navigator.pop(
                                                                            context,
                                                                            true),
                                                                    child: Text(
                                                                        l10n.text(
                                                                            'allowView')))
                                                              ]));
                                                  if (confirmed == true &&
                                                      mounted) {
                                                    await run((c) => c.command(
                                                        'expand',
                                                        {'epoch': epoch}));
                                                  }
                                                },
                                          icon: const Icon(Icons.open_in_full),
                                          tooltip: l10n.text('expandViewport')),
                                      const Spacer(),
                                      Flexible(
                                          child: Text(l10n.text('screenTap'))),
                                      Switch(
                                          value: clicks,
                                          onChanged: busy || pixels == null
                                              ? null
                                              : (value) => setState(
                                                  () => clicks = value)),
                                    ]),
                                    if (pixels != null && frame != null)
                                      LayoutBuilder(
                                          builder: (context, constraints) {
                                        final view = frame!['view']
                                            as Map<String, dynamic>;
                                        final width =
                                            (view['width'] as num).toDouble();
                                        final height =
                                            (view['height'] as num).toDouble();
                                        return AspectRatio(
                                            aspectRatio: width / height,
                                            child: GestureDetector(
                                                onTapUp: !clicks || busy
                                                    ? null
                                                    : (details) {
                                                        final x = (details
                                                                        .localPosition
                                                                        .dx /
                                                                    constraints
                                                                        .maxWidth *
                                                                    width)
                                                                .floor() +
                                                            (view['x'] as int);
                                                        final y = (details
                                                                        .localPosition
                                                                        .dy /
                                                                    constraints
                                                                        .maxWidth *
                                                                    width)
                                                                .floor() +
                                                            (view['y'] as int);
                                                        final revision =
                                                            frame!['revision']
                                                                as String;
                                                        unawaited(run((c) =>
                                                            c.click(epoch, x, y,
                                                                revision)));
                                                      },
                                                child: Image.memory(pixels!,
                                                    fit: BoxFit.fill,
                                                    gaplessPlayback: false)));
                                      }),
                                  ]),
                            ),
                            const SizedBox(height: 16),
                            CarryCard(
                              child: Column(
                                  crossAxisAlignment: CrossAxisAlignment.start,
                                  children: [
                                    _SectionTitle(
                                        icon: Icons.lock_outline,
                                        title: l10n.text('privateFill')),
                                    DropdownButtonFormField<String>(
                                        key: ValueKey(field),
                                        initialValue: field,
                                        isExpanded: true,
                                        decoration: InputDecoration(
                                            labelText:
                                                l10n.text('privateFillTitle')),
                                        items: fields
                                            .asMap()
                                            .entries
                                            .map((entry) => DropdownMenuItem(
                                                value:
                                                    entry.value['id'] as String,
                                                child: Text('${{
                                                      'account': l10n
                                                          .text('accountField'),
                                                      'password':
                                                          l10n.text('password'),
                                                      'otp': l10n.text(
                                                          'verificationCode')
                                                    }[entry.value['kind']] ?? l10n.text('field')} ${entry.key + 1}')))
                                            .toList(),
                                        onChanged: busy
                                            ? null
                                            : (value) => setState(() {
                                                  input.clear();
                                                  field = value;
                                                })),
                                    const SizedBox(height: 12),
                                    TextField(
                                        controller: input,
                                        obscureText: true,
                                        autocorrect: false,
                                        enableSuggestions: false,
                                        enableIMEPersonalizedLearning: false,
                                        maxLength: 4096,
                                        enabled: !busy,
                                        decoration: InputDecoration(
                                            labelText:
                                                l10n.text('privateEntry'),
                                            border: const OutlineInputBorder()),
                                        onSubmitted: (_) => send()),
                                    SizedBox(
                                        width: double.infinity,
                                        child: FilledButton.icon(
                                            onPressed: busy || field == null
                                                ? null
                                                : send,
                                            icon: const Icon(Icons.send),
                                            label: Text(l10n
                                                .text('fillTargetFields')))),
                                  ]),
                            ),
                            const SizedBox(height: 16),
                            CarryCard(
                              child: Column(
                                  crossAxisAlignment: CrossAxisAlignment.start,
                                  children: [
                                    _SectionTitle(
                                        icon: Icons.undo,
                                        title: l10n.text('returnComputer')),
                                    const SizedBox(height: 4),
                                    Text(l10n.text('loginDoneRevoke'),
                                        style: const TextStyle(
                                            color: CarryColors.muted,
                                            height: 1.4)),
                                    const SizedBox(height: 12),
                                    SizedBox(
                                        width: double.infinity,
                                        child: OutlinedButton(
                                            onPressed: busy ? null : handBack,
                                            child: Text(
                                                l10n.text('loginDoneReturn')))),
                                  ]),
                            ),
                          ],
                        ],
                        if (busy) const LinearProgressIndicator(),
                        if (message.isNotEmpty || messageKey != null)
                          Padding(
                              padding: const EdgeInsets.only(top: 12),
                              child: CarryNotice(
                                  text: messageKey == null
                                      ? message
                                      : l10n.text(messageKey!),
                                  isError: messageKey != 'connectionEnded')),
                      ])))),
    );
  }
}

class _StatusPill extends StatelessWidget {
  const _StatusPill({required this.label});
  final String label;
  @override
  Widget build(BuildContext context) => DecoratedBox(
        decoration: BoxDecoration(
            color: CarryColors.mint, borderRadius: BorderRadius.circular(999)),
        child: Padding(
            padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 7),
            child: Text(label,
                style: const TextStyle(
                    color: CarryColors.forest, fontWeight: FontWeight.w700))),
      );
}

class _SectionTitle extends StatelessWidget {
  const _SectionTitle({required this.icon, required this.title});
  final IconData icon;
  final String title;
  @override
  Widget build(BuildContext context) => Row(children: [
        Icon(icon, color: CarryColors.forest),
        const SizedBox(width: 8),
        Expanded(
            child: Text(title,
                style: const TextStyle(
                    fontSize: 18,
                    fontWeight: FontWeight.w700,
                    color: CarryColors.ink))),
      ]);
}

class _PairingIllustration extends StatelessWidget {
  const _PairingIllustration();
  @override
  Widget build(BuildContext context) {
    final l10n = AppLocalizations.of(context);
    return Wrap(
      alignment: WrapAlignment.center,
      crossAxisAlignment: WrapCrossAlignment.center,
      spacing: 12,
      runSpacing: 8,
      children: [
        const Row(
          mainAxisSize: MainAxisSize.min,
          children: [
            _IllustrationIcon(icon: Icons.phone_android_outlined),
            Padding(
              padding: EdgeInsets.symmetric(horizontal: 12),
              child: Icon(Icons.sync_alt, color: CarryColors.forest, size: 26),
            ),
            _IllustrationIcon(icon: Icons.laptop_mac_outlined),
          ],
        ),
        Text(l10n.text('manualTakeover'),
            style: const TextStyle(
                color: CarryColors.forest, fontWeight: FontWeight.w700)),
      ],
    );
  }
}

class _IllustrationIcon extends StatelessWidget {
  const _IllustrationIcon({required this.icon});
  final IconData icon;
  @override
  Widget build(BuildContext context) => DecoratedBox(
        decoration: const BoxDecoration(
            color: CarryColors.mint, shape: BoxShape.circle),
        child: Padding(
            padding: const EdgeInsets.all(14),
            child: Icon(icon, color: CarryColors.forest, size: 28)),
      );
}
