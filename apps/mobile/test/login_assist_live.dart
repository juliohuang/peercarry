// Run by native-e2e.mjs; ticket arrives on stdin and is never printed or saved.
import 'dart:convert';
import 'dart:io';
import 'package:peercarry_mobile/login_assist_client.dart';

Future<void> main() async {
  try {
    await check();
  } catch (error) {
    stderr.writeln(
        error is LoginAssistException ? error.code : error.runtimeType);
    exitCode = 1;
  }
}

Future<void> check() async {
  final line =
      await stdin.transform(utf8.decoder).transform(const LineSplitter()).first;
  final ticket = LoginTicket.parse(line);
  final client = LoginAssistClient(ticket);
  try {
    await client.pair(ticket);
    var state = await client.command('state');
    if (state['status']['phase'] != 'AwaitingUser') {
      throw StateError('initial state');
    }
    final epoch = state['status']['epoch'] as int;
    await client.command('approve', {'epoch': epoch});
    final fields = (state['fields'] as List).cast<Map<String, dynamic>>();
    final field = fields.firstWhere((f) => f['kind'] == 'otp')['id'] as String;
    state = await client.fill(epoch, field, '654321');
    if (state['status']['phase'] != 'Controlling') throw StateError('fill');
    state = await client.command('frame', {'epoch': epoch});
    if (state['data']['png'] is! String) throw StateError('capture');
    await client.disconnect();
    stdout.writeln('PASS');
  } finally {
    client.close();
  }
}
