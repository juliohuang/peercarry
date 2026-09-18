import 'package:flutter_test/flutter_test.dart';
import 'package:peercarry_mobile/models.dart';

void main() {
  test('parses protocol entry schema including files', () {
    final entry = Entry.fromJson({
      'id': 'abc',
      'kind': 'files',
      'preview': 'a.txt',
      'files': [
        {'name': 'a.txt', 'path': '/tmp/a.txt', 'size': 4}
      ]
    });
    expect(entry.id, 'abc');
    expect(entry.files, ['a.txt']);
  });
  test('missing capabilities is treated as empty', () {
    expect(Hello.fromJson({'host': 'pc'}).capabilities, isEmpty);
  });
}
