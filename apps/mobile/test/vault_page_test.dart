import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'dart:async';
import 'package:peercarry_mobile/app_theme.dart';
import 'package:peercarry_mobile/vault_page.dart';
import 'support/localized_app.dart';

class FakeVaultController implements VaultController {
  FakeVaultController({bool available = true, List<VaultEntry>? entries})
      : isAvailable = available,
        entries = entries ?? [];

  @override
  bool isAvailable;
  final List<VaultEntry> entries;
  String? unlockedWith;
  int locks = 0;
  String? lastQuery;
  bool failUnlock = false;
  bool failList = false;
  bool failCreate = false;
  Completer<void>? unlockGate;

  @override
  Future<void> unlock(String masterPassword) async {
    unlockedWith = masterPassword;
    await unlockGate?.future;
    if (failUnlock) throw StateError('sensitive-backend-detail');
  }

  @override
  Future<void> lock() async => locks++;

  @override
  Future<List<VaultEntry>> list({String query = ''}) async {
    lastQuery = query;
    if (failList) throw StateError('private database path');
    final q = query.toLowerCase();
    return entries
        .where((entry) =>
            q.isEmpty ||
            [entry.name, entry.address, entry.username, ...entry.tags]
                .any((value) => value.toLowerCase().contains(q)))
        .toList();
  }

  @override
  Future<VaultEntry> create(VaultEntry entry) async {
    if (failCreate) throw StateError('sensitive-create-error');
    final created = copyEntry(entry, id: 'created-${entries.length}');
    entries.add(created);
    return created;
  }

  @override
  Future<VaultEntry> update(VaultEntry entry) async {
    final index = entries.indexWhere((item) => item.id == entry.id);
    if (index >= 0) entries[index] = entry;
    return entry;
  }

  @override
  Future<void> delete(String id) async =>
      entries.removeWhere((entry) => entry.id == id);
}

VaultEntry copyEntry(VaultEntry entry, {required String id, String? name}) =>
    VaultEntry(
      id: id,
      name: name ?? entry.name,
      category: entry.category,
      environment: entry.environment,
      address: entry.address,
      port: entry.port,
      username: entry.username,
      password: entry.password,
      tls: entry.tls,
      usage: entry.usage,
      tags: entry.tags,
      databaseName: entry.databaseName,
      expiresAt: entry.expiresAt,
      updatedAt: entry.updatedAt,
    );

VaultEntry sampleEntry({
  String id = 'db-1',
  String name = 'Production database',
  VaultCategory category = VaultCategory.database,
  String password = 'vault-secret-9841',
}) =>
    VaultEntry(
      id: id,
      name: name,
      category: category,
      environment: 'production',
      address: 'db.internal.example',
      port: 5432,
      username: 'vault-user',
      password: password,
      tls: true,
      usage: 'service access',
      tags: const ['payments', 'critical'],
      databaseName: 'ledger',
      updatedAt: DateTime.utc(2026, 1, 2),
    );

Future<void> pumpVault(WidgetTester tester, VaultController? controller) async {
  await tester.pumpWidget(localizedTestApp(
    theme: carryTheme(),
    home: VaultPage(controller: controller),
  ));
  await tester.pumpAndSettle();
}

Future<void> unlockVault(WidgetTester tester, String password) async {
  await tester.enterText(find.byType(TextField).first, password);
  await tester.tap(find.text('解锁'));
  await tester.pumpAndSettle();
}

void main() {
  testWidgets('filtering never transfers password visibility to another entry',
      (tester) async {
    final controller = FakeVaultController(entries: [
      sampleEntry(),
      sampleEntry(
          id: 'second', name: 'Second account', password: 'other-secret'),
    ]);
    await pumpVault(tester, controller);
    await unlockVault(tester, 'test-master');
    await tester.tap(find.byTooltip('显示密码').first);
    await tester.pumpAndSettle();
    expect(find.text('vault-secret-9841'), findsOneWidget);
    await tester.enterText(find.byType(TextField).first, 'Second');
    await tester.pumpAndSettle();
    expect(find.text('other-secret'), findsNothing);
    expect(find.byTooltip('显示密码'), findsOneWidget);
  });

  testWidgets('backgrounding closes editor without popping the vault page',
      (tester) async {
    final controller = FakeVaultController(entries: [sampleEntry()]);
    await pumpVault(tester, controller);
    await unlockVault(tester, 'test-master');
    await tester.tap(find.byTooltip('编辑条目'));
    await tester.pumpAndSettle();
    expect(find.byType(AlertDialog), findsOneWidget);
    tester.binding.handleAppLifecycleStateChanged(AppLifecycleState.inactive);
    await tester.pumpAndSettle();
    expect(find.byType(AlertDialog), findsNothing);
    expect(find.byType(VaultPage), findsOneWidget);
    expect(find.text('解锁账号库'), findsOneWidget);
    expect(tester.takeException(), isNull);
  });

  testWidgets('missing and unavailable controllers keep the vault unavailable',
      (tester) async {
    await pumpVault(tester, null);
    expect(find.text('账号库服务尚未接入'), findsOneWidget);
    expect(find.text('解锁'), findsNothing);
    await pumpVault(tester, FakeVaultController(available: false));
    expect(find.text('账号库服务尚未接入'), findsOneWidget);
    expect(find.byType(TextField), findsNothing);
  });

  testWidgets(
      'unlock clears master password and masks secret fields without copy',
      (tester) async {
    final controller = FakeVaultController(entries: [sampleEntry()]);
    await pumpVault(tester, controller);
    final master = tester.widget<TextField>(find.byType(TextField).first);
    expect(master.obscureText, isTrue);
    expect(master.enableSuggestions, isFalse);
    expect(master.autocorrect, isFalse);
    await unlockVault(tester, 'master-passphrase');

    expect(controller.unlockedWith, 'master-passphrase');
    expect(
        tester.widget<TextField>(find.byType(TextField).first).controller!.text,
        isEmpty);
    expect(find.text('vault-secret-9841'), findsNothing);
    expect(find.byTooltip('显示密码'), findsOneWidget);
    expect(find.byTooltip('复制'), findsNothing);
    expect(find.byTooltip('复制密码'), findsNothing);

    await tester.tap(find.byTooltip('显示密码'));
    await tester.pumpAndSettle();
    expect(find.text('vault-secret-9841'), findsOneWidget);
    await tester.tap(find.byTooltip('隐藏密码'));
    await tester.pumpAndSettle();
    expect(find.text('vault-secret-9841'), findsNothing);
  });

  testWidgets('search and category chips filter controller results',
      (tester) async {
    final controller = FakeVaultController(entries: [
      sampleEntry(),
      sampleEntry(
          id: 'web-1', name: 'Admin portal', category: VaultCategory.website),
    ]);
    await pumpVault(tester, controller);
    await unlockVault(tester, 'test-master');
    expect(find.text('Production database'), findsOneWidget);
    expect(find.text('Admin portal'), findsOneWidget);

    await tester.enterText(find.byType(TextField).first, 'portal');
    await tester.pumpAndSettle();
    expect(controller.lastQuery, 'portal');
    expect(find.text('Production database'), findsNothing);
    expect(find.text('Admin portal'), findsOneWidget);

    await tester.tap(find.widgetWithText(ChoiceChip, '数据库'));
    await tester.pumpAndSettle();
    expect(find.text('Admin portal'), findsNothing);
  });

  testWidgets('create, update, and delete call the fake controller',
      (tester) async {
    final controller = FakeVaultController(entries: [sampleEntry()]);
    await pumpVault(tester, controller);
    await unlockVault(tester, 'test-master');

    await tester.tap(find.text('新增'));
    await tester.pumpAndSettle();
    await tester.enterText(
        find.widgetWithText(TextFormField, '名称'), 'New service');
    await tester.enterText(find.widgetWithText(TextFormField, '环境'), 'staging');
    await tester.enterText(
        find.widgetWithText(TextFormField, '地址'), 'svc.local');
    await tester.enterText(
        find.widgetWithText(TextFormField, '密码'), 'created-secret');
    await tester.tap(find.text('保存'));
    await tester.pumpAndSettle();
    expect(controller.entries, hasLength(2));
    expect(controller.entries.last.password, 'created-secret');

    await tester.tap(find.byTooltip('编辑条目').first);
    await tester.pumpAndSettle();
    await tester.enterText(
        find.widgetWithText(TextFormField, '名称'), 'Updated database');
    await tester.tap(find.text('保存'));
    await tester.pumpAndSettle();
    expect(controller.entries.first.name, 'Updated database');

    await tester.tap(find.byTooltip('删除条目').first);
    await tester.pumpAndSettle();
    await tester.tap(find.widgetWithText(FilledButton, '删除'));
    await tester.pumpAndSettle();
    expect(controller.entries, hasLength(1));
  });

  testWidgets('backgrounding locks and clears the displayed vault',
      (tester) async {
    final controller = FakeVaultController(entries: [sampleEntry()]);
    await pumpVault(tester, controller);
    await unlockVault(tester, 'test-master');
    expect(find.text('Production database'), findsOneWidget);

    tester.binding.handleAppLifecycleStateChanged(AppLifecycleState.inactive);
    await tester.pumpAndSettle();
    expect(controller.locks, 1);
    expect(find.text('Production database'), findsNothing);
    expect(find.text('解锁账号库'), findsOneWidget);
  });

  testWidgets('backgrounding during unlock invalidates the pending result',
      (tester) async {
    final controller = FakeVaultController(entries: [sampleEntry()])
      ..unlockGate = Completer<void>();
    await pumpVault(tester, controller);
    await tester.enterText(find.byType(TextField).first, 'pending-master');
    await tester.tap(find.text('解锁'));
    await tester.pump();

    tester.binding.handleAppLifecycleStateChanged(AppLifecycleState.inactive);
    await tester.pump();
    controller.unlockGate!.complete();
    await tester.pumpAndSettle();

    expect(find.text('Production database'), findsNothing);
    expect(find.text('解锁账号库'), findsOneWidget);
    expect(controller.locks, 1);
    expect(tester.takeException(), isNull);
  });

  testWidgets('backend exceptions show fixed text without exception details',
      (tester) async {
    final controller = FakeVaultController()..failUnlock = true;
    await pumpVault(tester, controller);
    await unlockVault(tester, 'wrong-master');
    expect(find.text('解锁失败，请检查主密码后重试。'), findsOneWidget);
    expect(find.textContaining('sensitive-backend-detail'), findsNothing);
    expect(tester.takeException(), isNull);

    controller.failUnlock = false;
    controller.failList = true;
    await unlockVault(tester, 'valid-master');
    expect(find.text('读取失败，请稍后重试。'), findsOneWidget);
    expect(find.textContaining('private database path'), findsNothing);
    expect(tester.takeException(), isNull);
  });
}
