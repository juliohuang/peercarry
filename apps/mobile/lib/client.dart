import 'dart:async';
import 'dart:convert';
import 'dart:io';
import 'dart:typed_data';
import 'package:crypto/crypto.dart';
import 'package:file_selector/file_selector.dart';
import 'package:http/http.dart' as http;
import 'package:uuid/uuid.dart';
import 'models.dart';

class SyncHttpException implements Exception {
  SyncHttpException(this.statusCode);
  final int statusCode;
  @override
  String toString() => '服务返回 HTTP $statusCode';
}

class _PendingUpload {
  _PendingUpload(this.requestId);
  final String requestId;
  String? id;
}

class SyncClient {
  SyncClient(
      {required String baseUrl,
      this.sharedToken,
      required this.deviceId,
      this.deviceToken,
      http.Client? transport,
      this.requestTimeout = const Duration(seconds: 30)})
      : baseUrl = _normalizeBase(baseUrl),
        _transport = transport ?? http.Client();
  final String baseUrl;
  final String? sharedToken;
  final String deviceId;
  final String? deviceToken;
  final Duration requestTimeout;
  final http.Client _transport;
  final _pending = <String, _PendingUpload>{};
  static const chunkSize = 1024 * 1024;

  static String _normalizeBase(String value) {
    final uri = Uri.tryParse(value.trim());
    if (uri == null ||
        !['http', 'https'].contains(uri.scheme) ||
        uri.host.isEmpty ||
        uri.userInfo.isNotEmpty ||
        uri.hasQuery ||
        uri.hasFragment ||
        (uri.path.isNotEmpty && uri.path != '/')) {
      throw const FormatException('请输入电脑的 http/https 地址，不含路径、账号或查询参数');
    }
    return uri.replace(path: '').toString();
  }

  void dispose() => _transport.close();
  Uri _uri(String path) => Uri.parse('$baseUrl$path');
  // Keep legacy x-syncclip-* wire headers for compatibility with existing peers.
  Map<String, String> _headers({bool device = false}) => {
        if (sharedToken?.isNotEmpty == true) 'x-syncclip-token': sharedToken!,
        if (device) 'x-syncclip-device-id': deviceId,
        if (device && deviceToken?.isNotEmpty == true)
          'x-syncclip-device-token': deviceToken!,
      };
  Future<http.Response> _request(String method, String path,
      {bool device = false,
      Object? json,
      List<int>? bytes,
      int? offset,
      Duration? timeout}) async {
    final request = http.Request(method, _uri(path))..followRedirects = false;
    request.headers.addAll(_headers(device: device));
    if (json != null) {
      request.headers['content-type'] = 'application/json';
      request.bodyBytes = utf8.encode(jsonEncode(json));
    }
    if (bytes != null) {
      request.headers['content-type'] = 'application/octet-stream';
      request.bodyBytes = bytes;
    }
    if (offset != null) request.headers['upload-offset'] = '$offset';
    return (() async {
      final response = await _transport.send(request);
      final data = BytesBuilder(copy: false);
      await for (final chunk in response.stream) {
        if (data.length + chunk.length > 16 * 1024 * 1024) {
          throw const FormatException('服务响应过大');
        }
        data.add(chunk);
      }
      return http.Response.bytes(data.takeBytes(), response.statusCode,
          headers: response.headers);
    })()
        .timeout(timeout ?? requestTimeout);
  }

  static void _check(http.Response response) {
    if (response.statusCode < 200 || response.statusCode >= 300) {
      throw SyncHttpException(response.statusCode);
    }
  }

  static Map<String, dynamic> _object(http.Response response) =>
      decodeObject(utf8.decode(response.bodyBytes));
  Future<Hello> hello() async {
    final r = await _request('GET', '/v1/hello');
    _check(r);
    final data = _object(r);
    if (data['protocol'] != 1) throw const FormatException('不支持该电脑的协议版本');
    return Hello.fromJson(data);
  }

  Future<List<Entry>> entries() async {
    final r = await _request('GET', '/v1/entries?limit=200');
    _check(r);
    final data = jsonDecode(utf8.decode(r.bodyBytes));
    if (data is! List) throw const FormatException('无效的共享列表');
    return data
        .map((e) => Entry.fromJson(Map<String, dynamic>.from(e as Map)))
        .toList();
  }

  Future<void> sendText(String text) async {
    final size = utf8.encode(text).length;
    if (text.trim().isEmpty || size > 65536) {
      throw const FormatException('文本不能为空或超过 64 KiB');
    }
    final body = {
      'id': const Uuid().v4(),
      'kind': 'text',
      'origin': {
        'host': 'mobile',
        'node_id': deviceId,
        'addr': baseUrl,
        'os': Platform.operatingSystem
      },
      'created_at': DateTime.now().toUtc().toIso8601String(),
      'text': text,
      'size': size
    };
    final r = await _request('POST', '/v1/entries', json: body);
    _check(r);
    if (_object(r)['id'] != body['id']) {
      throw const FormatException('电脑未确认该文本条目');
    }
  }

  static UploadStatus _validated(http.Response r, int size,
      {String? id, int minimum = 0, int? maximum}) {
    _check(r);
    final s = UploadStatus.fromJson(_object(r));
    if (s.size != size ||
        (id != null && s.id != id) ||
        s.offset < minimum ||
        s.offset > (maximum ?? size)) {
      throw const FormatException('上传响应中的进度或会话不匹配');
    }
    return s;
  }

  static bool _retryable(Object e) =>
      e is TimeoutException ||
      e is http.ClientException ||
      e is SocketException ||
      (e is SyncHttpException &&
          [408, 429, 500, 502, 503, 504].contains(e.statusCode));
  Future<UploadStatus> _status(String id, int size,
          {int minimum = 0, int? maximum}) async =>
      _validated(await _request('GET', '/v1/uploads/$id', device: true), size,
          id: id, minimum: minimum, maximum: maximum);

  /// Foreground transfer. Pending IDs survive retries within this client only.
  Future<UploadStatus> upload(XFile file,
      {void Function(int sent, int total)? onProgress}) async {
    final size = await file.length();
    final filename = file.name.replaceAll('\\', '/').split('/').last;
    final hash = (await sha256.bind(file.openRead()).first).toString();
    final key = '${file.path}\n${file.name}\n$size\n$hash';
    final pending =
        _pending.putIfAbsent(key, () => _PendingUpload(const Uuid().v4()));
    UploadStatus? status;
    for (var attempt = 0; attempt < 3; attempt++) {
      try {
        status = pending.id == null
            ? _validated(
                await _request('POST', '/v1/uploads', device: true, json: {
                  'request_id': pending.requestId,
                  'filename': filename,
                  'size': size,
                  'sha256': hash
                }),
                size)
            : await _status(pending.id!, size);
        pending.id = status.id;
        break;
      } catch (e) {
        if (!_retryable(e) || attempt == 2) rethrow;
      }
    }
    var current = status!;
    while (current.offset < size) {
      final start = current.offset;
      final end = (start + chunkSize < size) ? start + chunkSize : size;
      final builder = BytesBuilder(copy: false);
      await for (final chunk in file.openRead(start, end)) {
        if (builder.length + chunk.length > end - start) {
          throw const FormatException('文件读取大小不匹配');
        }
        builder.add(chunk);
      }
      final data = builder.takeBytes();
      if (data.length != end - start) {
        throw const FormatException('源文件已变化或读取提前结束');
      }
      var progressed = false;
      for (var attempt = 0; attempt < 3; attempt++) {
        try {
          current = _validated(
              await _request('PATCH', '/v1/uploads/${current.id}',
                  device: true,
                  bytes: data,
                  offset: start,
                  timeout: const Duration(seconds: 60)),
              size,
              id: current.id,
              minimum: end,
              maximum: end);
          progressed = true;
          break;
        } catch (e) {
          if (!_retryable(e) &&
              !(e is SyncHttpException && e.statusCode == 409)) {
            rethrow;
          }
          current =
              await _status(current.id, size, minimum: start, maximum: end);
          if (current.offset > start) {
            progressed = true;
            break;
          }
          if (attempt == 2) rethrow;
        }
      }
      if (!progressed) throw const FormatException('上传没有取得进展');
      onProgress?.call(current.offset, size);
    }
    if (current.state != 'completed') {
      for (var attempt = 0; attempt < 3; attempt++) {
        try {
          current = _validated(
              await _request('POST', '/v1/uploads/${current.id}/complete',
                  device: true, timeout: const Duration(seconds: 600)),
              size,
              id: current.id,
              minimum: size);
          if (current.state != 'completed' || current.entryId == null) {
            throw const FormatException('电脑尚未确认上传完成');
          }
          break;
        } catch (e) {
          if (!_retryable(e) || attempt == 2) rethrow;
        }
      }
    }
    _pending.remove(key);
    onProgress?.call(size, size);
    return current;
  }
}
