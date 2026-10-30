import 'dart:async';

import 'package:flutter/material.dart';

import 'app_theme.dart';
import 'app_localizations.dart';

enum VaultCategory { system, database, redis, website }

String _categoryLabel(VaultCategory value, AppLocalizations l10n) =>
    switch (value) {
      VaultCategory.system => l10n.text('systemCategory'),
      VaultCategory.database => l10n.text('databaseCategory'),
      VaultCategory.redis => 'Redis',
      VaultCategory.website => l10n.text('website'),
    };

class VaultEntry {
  const VaultEntry({
    required this.id,
    required this.name,
    required this.category,
    required this.environment,
    required this.address,
    this.port,
    required this.username,
    required this.password,
    required this.tls,
    required this.usage,
    required this.tags,
    required this.updatedAt,
    this.databaseName,
    this.expiresAt,
  });

  final String id;
  final String name;
  final VaultCategory category;
  final String environment;
  final String address;
  final int? port;
  final String username;
  final String password;
  final bool tls;
  final String usage;
  final List<String> tags;
  final String? databaseName;
  final DateTime? expiresAt;
  final DateTime updatedAt;
}

/// UI boundary for a separately implemented, authenticated encrypted vault.
/// This interface intentionally contains no HTTP or cryptographic behavior.
abstract interface class VaultController {
  bool get isAvailable;
  Future<void> unlock(String masterPassword);
  Future<void> lock();
  Future<List<VaultEntry>> list({String query = ''});
  Future<VaultEntry> create(VaultEntry entry);
  Future<VaultEntry> update(VaultEntry entry);
  Future<void> delete(String id);
}

class VaultPage extends StatefulWidget {
  const VaultPage({super.key, this.controller});

  final VaultController? controller;

  @override
  State<VaultPage> createState() => _VaultPageState();
}

class _VaultPageState extends State<VaultPage> with WidgetsBindingObserver {
  final _masterPassword = TextEditingController();
  final _search = TextEditingController();
  List<VaultEntry> _entries = const [];
  VaultCategory? _category;
  bool _unlocked = false;
  bool _busy = false;
  bool _pendingUnlock = false;
  bool _locking = false;
  bool _foreground = true;
  bool _modalOpen = false;
  int _generation = 0;
  String? _notice;
  bool _isError = false;

  VaultController? get _controller => widget.controller;
  bool get _available => _controller?.isAvailable ?? false;
  AppLocalizations get l10n => AppLocalizations.of(context);

  @override
  void initState() {
    super.initState();
    WidgetsBinding.instance.addObserver(this);
  }

  @override
  void didUpdateWidget(covariant VaultPage oldWidget) {
    super.didUpdateWidget(oldWidget);
    if (oldWidget.controller != widget.controller) {
      _generation++;
      _masterPassword.clear();
      if (_modalOpen) {
        _modalOpen = false;
        unawaited(Navigator.of(context).maybePop());
      }
      final wasUnlocked = _unlocked;
      setState(() {
        _unlocked = false;
        _busy = false;
        _entries = const [];
        _search.clear();
      });
      if (wasUnlocked && oldWidget.controller != null) {
        unawaited(oldWidget.controller!.lock().catchError((_) {}));
      }
    }
  }

  @override
  void didChangeAppLifecycleState(AppLifecycleState state) {
    _foreground = state == AppLifecycleState.resumed;
    if (state != AppLifecycleState.resumed) unawaited(_lock());
  }

  @override
  void dispose() {
    WidgetsBinding.instance.removeObserver(this);
    _masterPassword.clear();
    _search.clear();
    _masterPassword.dispose();
    _search.dispose();
    final controller = _controller;
    if (_unlocked && controller != null) {
      unawaited(controller.lock().catchError((_) {}));
    }
    super.dispose();
  }

  void _setNotice(String message, {bool error = false}) {
    if (!mounted) return;
    setState(() {
      _notice = message;
      _isError = error;
    });
  }

  Future<void> _unlock() async {
    final controller = _controller;
    if (!_available ||
        controller == null ||
        _busy ||
        _pendingUnlock ||
        _locking ||
        !_foreground) {
      return;
    }
    final secret = _masterPassword.text;
    _masterPassword.clear();
    if (secret.isEmpty) {
      _setNotice(l10n.text('enterMaster'), error: true);
      return;
    }
    final generation = ++_generation;
    _pendingUnlock = true;
    setState(() {
      _busy = true;
      _notice = null;
    });
    try {
      await controller.unlock(secret);
      if (!mounted || generation != _generation) {
        await controller.lock();
        return;
      }
      if (!mounted) return;
      setState(() {
        _unlocked = true;
        _busy = false;
      });
      _setNotice(l10n.text('vaultUnlocked'));
      await _refresh();
    } catch (_) {
      if (mounted && generation == _generation) {
        setState(() => _busy = false);
        _setNotice(l10n.text('unlockFailed'), error: true);
      }
    } finally {
      _pendingUnlock = false;
      if (mounted && identical(controller, _controller)) {
        setState(() => _busy = _locking);
      }
    }
  }

  Future<void> _lock({bool showNotice = true}) async {
    if (_locking) return;
    final wasUnlocked = _unlocked;
    _locking = wasUnlocked;
    _generation++;
    _masterPassword.clear();
    if (_modalOpen && mounted) {
      _modalOpen = false;
      unawaited(Navigator.of(context).maybePop());
    }
    setStateIfMounted(() {
      _unlocked = false;
      _busy = _locking || _pendingUnlock;
      _entries = const [];
      _search.clear();
    });
    final controller = _controller;
    if (wasUnlocked && controller != null) {
      try {
        await controller.lock();
      } catch (_) {
        if (showNotice) _setNotice(l10n.text('lockFailed'), error: true);
        return;
      } finally {
        _locking = false;
        if (mounted) setState(() => _busy = _pendingUnlock);
      }
    }
    if (showNotice && wasUnlocked) _setNotice(l10n.text('vaultLocked'));
  }

  // Avoid setState after disposal in lifecycle and widget replacement paths.
  void setStateIfMounted(VoidCallback callback) {
    if (mounted) setState(callback);
  }

  Future<void> _refresh() async {
    final controller = _controller;
    if (!_unlocked || controller == null) return;
    final generation = ++_generation;
    try {
      final entries = await controller.list(query: _search.text.trim());
      if (mounted && _unlocked && generation == _generation) {
        setState(() => _entries = entries);
      }
    } catch (_) {
      if (mounted && _unlocked && generation == _generation) {
        _setNotice(l10n.text('readFailed'), error: true);
      }
    }
  }

  Future<void> _edit([VaultEntry? existing]) async {
    final controller = _controller;
    final generation = _generation;
    if (controller == null || !_unlocked) return;
    _modalOpen = true;
    final entry = await showDialog<VaultEntry>(
      context: context,
      builder: (_) => _VaultEntryDialog(entry: existing),
    );
    _modalOpen = false;
    if (entry == null || !mounted) return;
    if (!_unlocked ||
        generation != _generation ||
        !identical(controller, _controller)) {
      return;
    }
    try {
      if (existing == null) {
        await controller.create(entry);
      } else {
        await controller.update(entry);
      }
      await _refresh();
      if (mounted && _unlocked && identical(controller, _controller)) {
        _setNotice(existing == null
            ? l10n.text('entryAdded')
            : l10n.text('entrySaved'));
      }
    } catch (_) {
      _setNotice(l10n.text('saveFailed'), error: true);
    }
  }

  Future<void> _delete(VaultEntry entry) async {
    final controller = _controller;
    if (controller == null || !_unlocked) return;
    final generation = _generation;
    _modalOpen = true;
    final confirmed = await showDialog<bool>(
      context: context,
      builder: (context) => AlertDialog(
        title: Text(l10n.text('deleteConfirm')),
        content: Text(l10n.format('deleteEntryBody', {'name': entry.name})),
        actions: [
          TextButton(
              onPressed: () => Navigator.pop(context, false),
              child: Text(l10n.text('cancel'))),
          FilledButton(
              onPressed: () => Navigator.pop(context, true),
              child: Text(l10n.text('delete'))),
        ],
      ),
    );
    _modalOpen = false;
    if (confirmed != true ||
        !mounted ||
        !_unlocked ||
        generation != _generation ||
        !identical(controller, _controller)) {
      return;
    }
    try {
      await controller.delete(entry.id);
      if (!mounted ||
          !_unlocked ||
          generation != _generation ||
          !identical(controller, _controller)) {
        return;
      }
      await _refresh();
      if (mounted) _setNotice(l10n.text('entryDeleted'));
    } catch (_) {
      _setNotice(l10n.text('deleteFailed'), error: true);
    }
  }

  List<VaultEntry> get _visibleEntries => _entries
      .where((entry) => _category == null || entry.category == _category)
      .toList(growable: false);

  @override
  Widget build(BuildContext context) => Scaffold(
        appBar: AppBar(
          title: Text(l10n.text('vaultTitle')),
          actions: [
            if (_unlocked)
              IconButton(
                tooltip: l10n.text('vaultLock'),
                onPressed: _busy ? null : () => unawaited(_lock()),
                icon: const Icon(Icons.lock_outline),
              ),
          ],
        ),
        body: SafeArea(
          child: Center(
            child: ConstrainedBox(
              constraints: const BoxConstraints(maxWidth: 680),
              child: ListView(
                padding: const EdgeInsets.fromLTRB(20, 12, 20, 32),
                children: [
                  const _VaultHeader(),
                  const SizedBox(height: 16),
                  if (!_available) _unavailableCard(),
                  if (_available && !_unlocked) _unlockCard(),
                  if (_available && _unlocked) _listView(),
                  if (_notice != null) ...[
                    const SizedBox(height: 14),
                    CarryNotice(
                      text: _notice!,
                      isError: _isError,
                      icon: _isError
                          ? Icons.error_outline
                          : Icons.check_circle_outline,
                    ),
                  ],
                ],
              ),
            ),
          ),
        ),
      );

  Widget _unavailableCard() => CarryCard(
        child: Column(crossAxisAlignment: CrossAxisAlignment.start, children: [
          const Icon(Icons.lock_clock_outlined,
              color: CarryColors.forest, size: 30),
          const SizedBox(height: 14),
          Text(l10n.text('vaultNotConnected'),
              style:
                  const TextStyle(fontSize: 18, fontWeight: FontWeight.w700)),
          const SizedBox(height: 6),
          Text(l10n.text('vaultUnavailable'),
              style: const TextStyle(color: CarryColors.muted, height: 1.5)),
        ]),
      );

  Widget _unlockCard() => CarryCard(
        child: Column(crossAxisAlignment: CrossAxisAlignment.start, children: [
          const Icon(Icons.shield_outlined,
              color: CarryColors.forest, size: 30),
          const SizedBox(height: 14),
          Text(l10n.text('unlockVault'),
              style:
                  const TextStyle(fontSize: 18, fontWeight: FontWeight.w700)),
          const SizedBox(height: 6),
          Text(l10n.text('enterMasterPassword'),
              style: const TextStyle(color: CarryColors.muted)),
          const SizedBox(height: 18),
          TextField(
            controller: _masterPassword,
            obscureText: true,
            enableSuggestions: false,
            autocorrect: false,
            textInputAction: TextInputAction.done,
            onSubmitted: (_) => unawaited(_unlock()),
            decoration: InputDecoration(
              labelText: l10n.text('masterPassword'),
              prefixIcon: const Icon(Icons.key_outlined),
            ),
          ),
          const SizedBox(height: 14),
          SizedBox(
            width: double.infinity,
            child: FilledButton(
              onPressed: _busy ? null : () => unawaited(_unlock()),
              child: _busy
                  ? const SizedBox(
                      width: 20,
                      height: 20,
                      child: CircularProgressIndicator(strokeWidth: 2))
                  : Text(l10n.text('unlock')),
            ),
          ),
        ]),
      );

  Widget _listView() => Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Row(children: [
            Expanded(
              child: Text(l10n.text('accountEntries'),
                  style: const TextStyle(
                      fontSize: 20, fontWeight: FontWeight.w700)),
            ),
            FilledButton.icon(
              onPressed: _busy ? null : () => unawaited(_edit()),
              icon: const Icon(Icons.add, size: 18),
              label: Text(l10n.text('addNew')),
            ),
          ]),
          const SizedBox(height: 14),
          TextField(
            controller: _search,
            enableSuggestions: false,
            autocorrect: false,
            onChanged: (_) => unawaited(_refresh()),
            decoration: InputDecoration(
              hintText: l10n.text('searchVault'),
              prefixIcon: const Icon(Icons.search),
              suffixIcon: IconButton(
                tooltip: l10n.text('clearSearch'),
                onPressed: () {
                  _search.clear();
                  unawaited(_refresh());
                },
                icon: const Icon(Icons.close),
              ),
            ),
          ),
          const SizedBox(height: 10),
          Wrap(spacing: 8, runSpacing: 4, children: [
            _categoryChip(null, l10n.text('all')),
            for (final category in VaultCategory.values)
              _categoryChip(category, _categoryLabel(category, l10n)),
          ]),
          const SizedBox(height: 12),
          if (_visibleEntries.isEmpty)
            CarryCard(
              child: Center(
                child: Text(l10n.text('noMatchingEntries'),
                    style: const TextStyle(color: CarryColors.muted)),
              ),
            )
          else
            for (final entry in _visibleEntries) ...[
              _entryCard(entry),
              const SizedBox(height: 10),
            ],
        ],
      );

  Widget _categoryChip(VaultCategory? value, String label) => ChoiceChip(
        label: Text(label),
        selected: _category == value,
        onSelected: (_) => setState(() => _category = value),
      );

  Widget _entryCard(VaultEntry entry) => _VaultEntryCard(
        key: ValueKey(entry.id),
        entry: entry,
        onEdit: () => unawaited(_edit(entry)),
        onDelete: () => unawaited(_delete(entry)),
      );
}

class _VaultHeader extends StatelessWidget {
  const _VaultHeader();

  @override
  Widget build(BuildContext context) {
    final l10n = AppLocalizations.of(context);
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Text(l10n.text('vaultPrivacyTitle'),
            style: const TextStyle(fontSize: 24, fontWeight: FontWeight.w700)),
        const SizedBox(height: 6),
        Text(l10n.text('vaultPrivacy'),
            style: const TextStyle(color: CarryColors.muted, height: 1.4)),
      ],
    );
  }
}

class _VaultEntryCard extends StatefulWidget {
  const _VaultEntryCard({
    super.key,
    required this.entry,
    required this.onEdit,
    required this.onDelete,
  });

  final VaultEntry entry;
  final VoidCallback onEdit;
  final VoidCallback onDelete;

  @override
  State<_VaultEntryCard> createState() => _VaultEntryCardState();
}

class _VaultEntryCardState extends State<_VaultEntryCard> {
  bool _revealPassword = false;
  AppLocalizations get l10n => AppLocalizations.of(context);

  @override
  void didUpdateWidget(covariant _VaultEntryCard oldWidget) {
    super.didUpdateWidget(oldWidget);
    if (oldWidget.entry.id != widget.entry.id ||
        oldWidget.entry.password != widget.entry.password) {
      _revealPassword = false;
    }
  }

  @override
  Widget build(BuildContext context) {
    final entry = widget.entry;
    return CarryCard(
      padding: const EdgeInsets.all(16),
      child: Column(crossAxisAlignment: CrossAxisAlignment.start, children: [
        Row(crossAxisAlignment: CrossAxisAlignment.start, children: [
          const Icon(Icons.key_rounded, color: CarryColors.forest),
          const SizedBox(width: 10),
          Expanded(
            child:
                Column(crossAxisAlignment: CrossAxisAlignment.start, children: [
              Text(entry.name,
                  style: const TextStyle(
                      fontWeight: FontWeight.w700, fontSize: 16)),
              const SizedBox(height: 3),
              Text(
                  '${_categoryLabel(entry.category, l10n)} · ${entry.environment}',
                  style:
                      const TextStyle(color: CarryColors.muted, fontSize: 12)),
            ]),
          ),
          IconButton(
            tooltip: l10n.text('editEntry'),
            onPressed: widget.onEdit,
            icon: const Icon(Icons.edit_outlined),
          ),
          IconButton(
            tooltip: l10n.text('deleteEntry'),
            onPressed: widget.onDelete,
            icon: const Icon(Icons.delete_outline),
          ),
        ]),
        const SizedBox(height: 10),
        _detail(
            l10n.text('address'),
            entry.port == null
                ? entry.address
                : '${entry.address}:${entry.port}'),
        if (entry.username.isNotEmpty)
          _detail(l10n.text('username'), entry.username),
        Row(children: [
          Expanded(
              child: _detail(l10n.text('password'),
                  _revealPassword ? entry.password : '••••••••')),
          IconButton(
            tooltip: _revealPassword
                ? l10n.text('hidePassword')
                : l10n.text('showPassword'),
            onPressed: () => setState(() => _revealPassword = !_revealPassword),
            icon:
                Icon(_revealPassword ? Icons.visibility_off : Icons.visibility),
          ),
        ]),
        if (entry.databaseName?.isNotEmpty == true)
          _detail(l10n.text('database'), entry.databaseName!),
        if (entry.tls)
          _detail(l10n.text('tlsEnabled'), l10n.text('tlsEnabledValue')),
        if (entry.usage.isNotEmpty)
          _detail(l10n.text('usageOptional'), entry.usage),
        if (entry.tags.isNotEmpty)
          _detail(l10n.text('tagsOptional'), entry.tags.join(' · ')),
        if (entry.expiresAt != null)
          _detail(
              l10n.text('expiresReminder'),
              MaterialLocalizations.of(context)
                  .formatShortDate(entry.expiresAt!)),
      ]),
    );
  }

  Widget _detail(String label, String value) => Padding(
        padding: const EdgeInsets.symmetric(vertical: 4),
        child: Row(crossAxisAlignment: CrossAxisAlignment.start, children: [
          SizedBox(
            width: 72,
            child:
                Text(label, style: const TextStyle(color: CarryColors.muted)),
          ),
          Expanded(child: Text(value)),
        ]),
      );
}

class _VaultEntryDialog extends StatefulWidget {
  const _VaultEntryDialog({this.entry});
  final VaultEntry? entry;

  @override
  State<_VaultEntryDialog> createState() => _VaultEntryDialogState();
}

class _VaultEntryDialogState extends State<_VaultEntryDialog>
    with WidgetsBindingObserver {
  AppLocalizations get l10n => AppLocalizations.of(context);
  final _formKey = GlobalKey<FormState>();
  late final TextEditingController _name;
  late final TextEditingController _environment;
  late final TextEditingController _address;
  late final TextEditingController _port;
  late final TextEditingController _username;
  late final TextEditingController _password;
  late final TextEditingController _database;
  late final TextEditingController _usage;
  late final TextEditingController _tags;
  late VaultCategory _category;
  bool _tls = false;

  @override
  void initState() {
    super.initState();
    WidgetsBinding.instance.addObserver(this);
    final entry = widget.entry;
    _name = TextEditingController(text: entry?.name ?? '');
    _environment = TextEditingController(text: entry?.environment ?? '');
    _address = TextEditingController(text: entry?.address ?? '');
    _port = TextEditingController(
        text: entry?.port == null ? '' : '${entry!.port}');
    _username = TextEditingController(text: entry?.username ?? '');
    _password = TextEditingController(text: entry?.password ?? '');
    _database = TextEditingController(text: entry?.databaseName ?? '');
    _usage = TextEditingController(text: entry?.usage ?? '');
    _tags = TextEditingController(text: entry?.tags.join(', ') ?? '');
    _category = entry?.category ?? VaultCategory.website;
    _tls = entry?.tls ?? false;
  }

  @override
  void dispose() {
    WidgetsBinding.instance.removeObserver(this);
    for (final controller in [
      _name,
      _environment,
      _address,
      _port,
      _username,
      _password,
      _database,
      _usage,
      _tags,
    ]) {
      controller.clear();
      controller.dispose();
    }
    super.dispose();
  }

  @override
  void didChangeAppLifecycleState(AppLifecycleState state) {
    if (state != AppLifecycleState.resumed && mounted) {
      _password.clear();
    }
  }

  TextFormField _field(String label, TextEditingController controller,
          {bool secret = false,
          bool required = false,
          TextInputType? keyboardType}) =>
      TextFormField(
        controller: controller,
        obscureText: secret,
        enableSuggestions: false,
        autocorrect: false,
        keyboardType: keyboardType,
        decoration: InputDecoration(labelText: label),
        validator: (value) {
          if (required && (value == null || value.trim().isEmpty)) {
            return l10n.format('requiredField', {'name': label});
          }
          if (controller == _port && value != null && value.trim().isNotEmpty) {
            final port = int.tryParse(value.trim());
            if (port == null || port < 1 || port > 65535) {
              return l10n.text('invalidPort');
            }
          }
          return null;
        },
      );

  void _save() {
    if (!_formKey.currentState!.validate()) return;
    final now = DateTime.now();
    final entry = VaultEntry(
      id: widget.entry?.id ?? '',
      name: _name.text.trim(),
      category: _category,
      environment: _environment.text.trim(),
      address: _address.text.trim(),
      port: int.tryParse(_port.text.trim()),
      username: _username.text.trim(),
      password: _password.text,
      databaseName:
          _database.text.trim().isEmpty ? null : _database.text.trim(),
      tls: _tls,
      usage: _usage.text.trim(),
      tags: _tags.text
          .split(',')
          .map((tag) => tag.trim())
          .where((tag) => tag.isNotEmpty)
          .toList(),
      expiresAt: widget.entry?.expiresAt,
      updatedAt: now,
    );
    Navigator.pop(context, entry);
  }

  @override
  Widget build(BuildContext context) => AlertDialog(
        title: Text(widget.entry == null
            ? l10n.text('newEntry')
            : l10n.text('editAccountEntry')),
        content: SizedBox(
          width: 480,
          child: SingleChildScrollView(
            child: Form(
              key: _formKey,
              child: Column(mainAxisSize: MainAxisSize.min, children: [
                DropdownButtonFormField<VaultCategory>(
                  initialValue: _category,
                  decoration: InputDecoration(labelText: l10n.text('category')),
                  items: [
                    for (final category in VaultCategory.values)
                      DropdownMenuItem(
                          value: category,
                          child: Text(_categoryLabel(category, l10n))),
                  ],
                  onChanged: (value) =>
                      setState(() => _category = value ?? _category),
                ),
                _field(l10n.text('name'), _name, required: true),
                _field(l10n.text('environment'), _environment),
                _field(l10n.text('address'), _address, required: true),
                _field(l10n.text('portOptional'), _port,
                    keyboardType: TextInputType.number),
                _field(l10n.text('username'), _username),
                _field(l10n.text('password'), _password, secret: true),
                _field(l10n.text('databaseOptional'), _database),
                _field(l10n.text('usageOptional'), _usage),
                _field(l10n.text('tagsOptional'), _tags),
                SwitchListTile(
                  contentPadding: EdgeInsets.zero,
                  title: Text(l10n.text('enableTls')),
                  value: _tls,
                  onChanged: (value) => setState(() => _tls = value),
                ),
              ]),
            ),
          ),
        ),
        actions: [
          TextButton(
              onPressed: () => Navigator.pop(context),
              child: Text(l10n.text('cancel'))),
          FilledButton(onPressed: _save, child: Text(l10n.text('save'))),
        ],
      );
}
