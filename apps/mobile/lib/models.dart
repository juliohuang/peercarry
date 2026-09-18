import 'dart:convert';

class Hello {
  const Hello(
      {required this.capabilities, required this.host, required this.version});
  final List<String> capabilities;
  final String host;
  final String version;
  factory Hello.fromJson(Map<String, dynamic> json) => Hello(
        capabilities: (json['capabilities'] as List? ?? const [])
            .whereType<String>()
            .toList(),
        host: json['host'] as String? ?? '',
        version: json['version'] as String? ?? '',
      );
}

class Entry {
  const Entry(
      {required this.id,
      required this.kind,
      required this.preview,
      this.text,
      this.files = const []});
  final String id;
  final String kind;
  final String preview;
  final String? text;
  final List<String> files;
  factory Entry.fromJson(Map<String, dynamic> json) {
    final files = (json['files'] as List? ?? const [])
        .whereType<Map>()
        .map((f) => f['name'] as String? ?? '')
        .toList();
    return Entry(
        id: json['id'] as String? ?? '',
        kind: json['kind'] as String? ?? '',
        preview: json['preview'] as String? ?? '',
        text: json['text'] as String?,
        files: files);
  }
  bool get isText => kind == 'text' && text != null;
  String get display =>
      isText ? text! : (preview.isNotEmpty ? preview : files.join(', '));
}

class UploadStatus {
  const UploadStatus(
      {required this.id,
      required this.offset,
      required this.size,
      required this.state,
      this.entryId});
  final String id;
  final int offset;
  final int size;
  final String state;
  final String? entryId;
  factory UploadStatus.fromJson(Map<String, dynamic> json) {
    final id = json['id'],
        offset = json['offset'],
        size = json['size'],
        state = json['state'];
    final entryId = json['entry_id'];
    final uuid = RegExp(
        r'^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$');
    if (id is! String ||
        !uuid.hasMatch(id) ||
        offset is! int ||
        size is! int ||
        offset < 0 ||
        size < offset ||
        (state != 'uploading' && state != 'completed') ||
        (state == 'completed' &&
            (offset != size ||
                entryId is! String ||
                !uuid.hasMatch(entryId)))) {
      throw const FormatException('无效的上传状态');
    }
    return UploadStatus(
        id: id,
        offset: offset,
        size: size,
        state: state as String,
        entryId: entryId as String?);
  }
}

Map<String, dynamic> decodeObject(String body) =>
    jsonDecode(body) as Map<String, dynamic>;
