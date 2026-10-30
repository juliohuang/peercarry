import 'dart:async';
import 'dart:convert';
import 'dart:io';
import 'dart:math';
import 'dart:typed_data';

import 'package:flutter/foundation.dart';
import 'package:http/http.dart' as http;

import 'client.dart';

enum ConnectionRoute { lan, relay }

class ConnectionTicket {
  ConnectionTicket._(this.host, this.lan, this.relay, this.certificate,
      this.secret, this.expiresAt);
  final String host;
  final List<Uri> lan;
  final Uri? relay;
  final Uint8List certificate;
  final String secret;
  final DateTime expiresAt;

  factory ConnectionTicket.parse(String text) {
    try {
      if (text.length > 16384) throw const FormatException();
      final data = jsonDecode(text) as Map<String, dynamic>;
      final host = data['host'] as String;
      final secret = data['secret'] as String;
      final certificate = base64Decode(data['certificate'] as String);
      final expiresAt = DateTime.fromMillisecondsSinceEpoch(
          (data['expires_at'] as int) * 1000,
          isUtc: true);
      final lan = (data['lan'] as List)
          .map((v) => _endpoint(v as String, lan: true))
          .toList();
      final relay = data['relay'] == null
          ? null
          : _endpoint(data['relay'] as String, lan: false);
      if (data['kind'] != 'peercarry-connect' ||
          data['version'] != 1 ||
          host.trim().isEmpty ||
          host.length > 255 ||
          host.runes.any((r) => r < 32) ||
          !_secretPattern.hasMatch(secret) ||
          certificate.isEmpty ||
          certificate.length > 4096 ||
          lan.length > 8 ||
          (lan.isEmpty && relay == null)) {
        throw const FormatException();
      }
      if (!expiresAt.isAfter(DateTime.now())) {
        throw const FormatException('配对码已过期，请在电脑上重新生成');
      }
      if (expiresAt.isAfter(DateTime.now().add(const Duration(minutes: 10)))) {
        throw const FormatException();
      }
      return ConnectionTicket._(
          host, List.unmodifiable(lan), relay, certificate, secret, expiresAt);
    } on FormatException catch (e) {
      if (e.message == '配对码已过期，请在电脑上重新生成') rethrow;
      throw const FormatException('不是有效的 PeerCarry 电脑连接码');
    } catch (_) {
      throw const FormatException('不是有效的 PeerCarry 电脑连接码');
    }
  }

  static final _secretPattern = RegExp(r'^[0-9a-f]{64}$');
  static Uri _endpoint(String text, {required bool lan}) {
    final uri = Uri.parse(text);
    if (text.length > 256 ||
        uri.scheme != 'https' ||
        uri.host.isEmpty ||
        uri.port == 0 ||
        uri.userInfo.isNotEmpty ||
        uri.hasQuery ||
        uri.hasFragment ||
        (uri.path.isNotEmpty && uri.path != '/')) {
      throw const FormatException();
    }
    final ip = InternetAddress.tryParse(uri.host);
    if (lan) {
      if (ip == null) throw const FormatException();
      final b = ip.rawAddress;
      final private = ip.type == InternetAddressType.IPv4 &&
          (b[0] == 10 ||
              (b[0] == 172 && b[1] >= 16 && b[1] <= 31) ||
              (b[0] == 192 && b[1] == 168));
      if (!private && !ip.isLoopback) throw const FormatException();
    }
    if ((Platform.isAndroid || Platform.isIOS) &&
        (ip?.isLoopback == true || uri.host.toLowerCase() == 'localhost')) {
      throw const FormatException();
    }
    return uri.replace(path: '');
  }
}

/// Standard TLS terminates at the desktop, including when FRP forwards it.
/// There is no trust in the phone's system CAs, proxy settings, or redirects.
class PairedConnection extends http.BaseClient {
  PairedConnection._(ConnectionTicket ticket)
      : host = ticket.host,
        _lan = ticket.lan,
        _relay = ticket.relay,
        _certificate = ticket.certificate;
  static const virtualOrigin = 'https://paired.peercarry.invalid';
  final String host;
  final List<Uri> _lan;
  final Uri? _relay;
  final Uint8List _certificate;
  final route = ValueNotifier<ConnectionRoute?>(null);
  String? _token;
  String? _deviceId;
  bool _closed = false;
  Uri? _last;
  DateTime _retryLanAt = DateTime.fromMillisecondsSinceEpoch(0);
  Future<void> _serial = Future.value();
  final _active = <HttpClient>{};

  static Future<PairedConnection> pair(ConnectionTicket ticket) async {
    final connection = PairedConnection._(ticket);
    final random = Random.secure();
    final requestId = List.generate(32, (_) => random.nextInt(256))
        .map((b) => b.toRadixString(16).padLeft(2, '0'))
        .join();
    try {
      final result = await connection._request(
          'POST',
          '/v1/pair',
          {},
          utf8.encode(jsonEncode({
            'secret': ticket.secret,
            'request_id': requestId,
            'name': Platform.isIOS
                ? 'iPhone'
                : Platform.isAndroid
                    ? 'Android 手机'
                    : '测试手机'
          })));
      if (result.statusCode != 200) {
        throw const FormatException('配对码已过期或已被使用，请在电脑上重新生成');
      }
      final data = jsonDecode(utf8.decode(result.bodyBytes)) as Map;
      final token = data['token'] as String;
      final id = data['device_id'] as String;
      if (!ConnectionTicket._secretPattern.hasMatch(token) ||
          !RegExp(r'^[0-9a-f-]{36}$').hasMatch(id) ||
          (data['expires_at'] as int) * 1000 <=
              DateTime.now().millisecondsSinceEpoch) {
        throw const FormatException('电脑配对响应无效');
      }
      connection._token = token;
      connection._deviceId = id;
      return connection;
    } catch (_) {
      connection.close();
      rethrow;
    }
  }

  PeerCarryClient createClient() => PeerCarryClient(
      baseUrl: virtualOrigin, deviceId: _deviceId!, transport: this);

  SecurityContext _context() {
    final context = SecurityContext(withTrustedRoots: false);
    if (Platform.isIOS) {
      context.setTrustedCertificatesBytes(_certificate);
    } else {
      final lines = RegExp(r'.{1,64}')
          .allMatches(base64Encode(_certificate))
          .map((m) => m.group(0)!)
          .join('\n');
      context.setTrustedCertificatesBytes(utf8.encode(
          '-----BEGIN CERTIFICATE-----\n$lines\n-----END CERTIFICATE-----\n'));
    }
    return context;
  }

  @override
  Future<http.StreamedResponse> send(http.BaseRequest request) {
    if (request.url.origin != virtualOrigin || _token == null || _closed) {
      throw http.ClientException('请重新扫码连接电脑');
    }
    final result = _serial.then((_) async {
      if (_closed) throw http.ClientException('连接已关闭');
      final body = await request.finalize().toBytes();
      if (body.length > 1024 * 1024) throw http.ClientException('请求过大');
      final response = await _request(
          request.method,
          '${request.url.path}${request.url.hasQuery ? '?${request.url.query}' : ''}',
          request.headers,
          body);
      return http.StreamedResponse(
          Stream.value(response.bodyBytes), response.statusCode,
          headers: response.headers, request: request);
    });
    _serial = result.then<void>((_) {}, onError: (Object _, StackTrace __) {});
    return result;
  }

  Future<http.Response> _request(String method, String path,
      Map<String, String> headers, List<int> body) async {
    final endpoints = <Uri>[];
    if (_last != _relay || DateTime.now().isAfter(_retryLanAt)) {
      if (_last != null && _lan.contains(_last)) endpoints.add(_last!);
      endpoints.addAll(_lan.where((u) => !endpoints.contains(u)));
    }
    if (_relay != null) endpoints.add(_relay);
    // While using a healthy relay, probe the LAN only every 30 seconds. If
    // the relay fails meanwhile, try LAN immediately rather than waiting.
    endpoints.addAll(_lan.where((u) => !endpoints.contains(u)));
    final complete = path.endsWith('/complete');
    final deadline = DateTime.now().add(Duration(seconds: complete ? 590 : 27));
    for (final endpoint in endpoints) {
      if (_closed) throw http.ClientException('连接已关闭');
      final remaining = deadline.difference(DateTime.now());
      if (remaining.isNegative) break;
      final client = HttpClient(context: _context());
      client.findProxy = (_) => 'DIRECT';
      client.connectionTimeout =
          Duration(milliseconds: endpoint == _relay ? 5000 : 1200);
      _active.add(client);
      try {
        final response = await (() async {
          final request = await client.openUrl(method, endpoint.resolve(path));
          request.followRedirects = false;
          // Forward only protocol headers. In particular, never forward a
          // legacy shared token to the LAN or public relay address.
          for (final key in ['content-type', 'upload-offset']) {
            if (headers[key] != null) request.headers.set(key, headers[key]!);
          }
          if (path == '/v1/pair') {
            request.headers.contentType = ContentType.json;
          }
          if (_token != null) {
            request.headers.set('authorization', 'Bearer $_token');
          }
          request.contentLength = body.length;
          request.add(body);
          final incoming = await request.close();
          final bytes = BytesBuilder(copy: false);
          await for (final chunk in incoming) {
            if (bytes.length + chunk.length > 16 * 1024 * 1024) {
              throw const FormatException('电脑响应过大');
            }
            bytes.add(chunk);
          }
          if (incoming.isRedirect) {
            throw const FormatException('连接地址发生重定向，请重新扫码');
          }
          return http.Response.bytes(bytes.takeBytes(), incoming.statusCode,
              headers: {
                HttpHeaders.contentTypeHeader:
                    incoming.headers.contentType?.toString() ??
                        'application/json'
              });
        })()
            .timeout(complete
                ? remaining
                : Duration(
                    milliseconds: min(remaining.inMilliseconds,
                        endpoint == _relay ? 12000 : 3000)));
        if (_closed) throw http.ClientException('连接已关闭');
        _last = endpoint;
        route.value =
            endpoint == _relay ? ConnectionRoute.relay : ConnectionRoute.lan;
        if (endpoint == _relay) {
          _retryLanAt = DateTime.now().add(const Duration(seconds: 30));
        }
        return response;
      } on FormatException {
        rethrow;
      } catch (error) {
        if (error is! SocketException &&
            error is! TlsException &&
            error is! HttpException &&
            error is! TimeoutException) {
          rethrow;
        }
        // Retry identical bytes on the other route. Entry IDs, upload request
        // IDs and upload offsets make ambiguous write responses recoverable.
      } finally {
        _active.remove(client);
        client.close(force: true);
      }
    }
    if (!_closed) route.value = null;
    throw http.ClientException('无法连接电脑，请确认电脑在线、局域网权限或中继配置');
  }

  @override
  void close() {
    if (_closed) return;
    _closed = true;
    _token = null;
    _deviceId = null;
    for (final client in _active) {
      client.close(force: true);
    }
    _active.clear();
    route.dispose();
    super.close();
  }
}
