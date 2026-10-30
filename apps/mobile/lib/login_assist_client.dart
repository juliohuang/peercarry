import 'dart:async';
import 'dart:convert';
import 'dart:io';
import 'dart:typed_data';

class LoginAssistException implements Exception {
  const LoginAssistException(this.code);
  final String code;
  @override
  String toString() => code;
}

class LoginTicket {
  LoginTicket._(this.url, this.certificate, this.secret);
  final Uri url;
  final Uint8List certificate;
  final String secret;

  factory LoginTicket.parse(String text) {
    try {
      if (text.length > 16384) throw const FormatException();
      final data = jsonDecode(text) as Map<String, dynamic>;
      final uri = Uri.parse(data['url'] as String);
      final secret = data['secret'] as String;
      final cert = base64Decode(data['certificate'] as String);
      if (data['version'] != 1 ||
          uri.scheme != 'https' ||
          InternetAddress.tryParse(uri.host) == null ||
          uri.userInfo.isNotEmpty ||
          uri.hasQuery ||
          uri.hasFragment ||
          uri.path.isNotEmpty ||
          !RegExp(r'^[0-9a-f]{64}$').hasMatch(secret) ||
          cert.isEmpty ||
          cert.length > 8192) {
        throw const FormatException();
      }
      if ((Platform.isAndroid || Platform.isIOS) &&
          InternetAddress(uri.host).isLoopback) {
        throw const FormatException();
      }
      return LoginTicket._(uri, cert, secret);
    } catch (_) {
      throw const LoginAssistException('INVALID_PAIRING');
    }
  }
}

class LoginAssistClient {
  LoginAssistClient(LoginTicket ticket)
      : _url = ticket.url,
        _client = HttpClient(context: _context(ticket.certificate)) {
    _client.connectionTimeout = const Duration(seconds: 8);
    _client.findProxy = (_) => 'DIRECT';
    _client.maxConnectionsPerHost = 1;
  }
  final Uri _url;
  final HttpClient _client;
  String? _token;
  bool _closed = false;
  int _command = 0;

  static SecurityContext _context(Uint8List certificate) {
    final context = SecurityContext(withTrustedRoots: false);
    // dart:io requires a single DER certificate on iOS; other platforms use PEM.
    if (Platform.isIOS) {
      context.setTrustedCertificatesBytes(certificate);
    } else {
      final encoded = base64Encode(certificate);
      final lines = RegExp(r'.{1,64}')
          .allMatches(encoded)
          .map((m) => m.group(0)!)
          .join('\n');
      context.setTrustedCertificatesBytes(utf8.encode(
          '-----BEGIN CERTIFICATE-----\n$lines\n-----END CERTIFICATE-----\n'));
    }
    return context;
  }

  Future<Map<String, dynamic>> _post(
      String path, Map<String, dynamic> body) async {
    if (_closed) throw const LoginAssistException('DISCONNECTED');
    try {
      return await (() async {
        final request = await _client.postUrl(_url.replace(path: path));
        request.followRedirects = false;
        request.headers.contentType = ContentType.json;
        if (_token != null) {
          request.headers.set('authorization', 'Bearer $_token');
        }
        request.add(utf8.encode(jsonEncode(body)));
        final response = await request.close();
        final bytes = BytesBuilder(copy: false);
        await for (final chunk in response) {
          if (bytes.length + chunk.length > 12 * 1024 * 1024) {
            throw const LoginAssistException('RESPONSE_TOO_LARGE');
          }
          bytes.add(chunk);
        }
        if (_closed) throw const LoginAssistException('DISCONNECTED');
        final data =
            jsonDecode(utf8.decode(bytes.takeBytes())) as Map<String, dynamic>;
        if (response.statusCode != 200) {
          const allowed = {
            'UNAUTHORIZED',
            'BUSY',
            'EXPIRED',
            'INVALID_STATE',
            'TARGET_CHANGED',
            'OUT_OF_SCOPE',
            'FILL_UNCONFIRMED',
            'CLICK_UNCONFIRMED',
            'REVOCATION_UNCONFIRMED',
            'REPLAY'
          };
          throw LoginAssistException(allowed.contains(data['error'])
              ? data['error'] as String
              : 'REQUEST_FAILED');
        }
        return data;
      })()
          .timeout(const Duration(seconds: 10));
    } on LoginAssistException {
      rethrow;
    } catch (_) {
      close();
      // Do not expose URLs, response bodies or TLS diagnostics containing tickets.
      throw const LoginAssistException('CONNECTION_FAILED');
    }
  }

  Future<void> pair(LoginTicket ticket) async {
    if (ticket.url != _url || _token != null) {
      throw const LoginAssistException('INVALID_PAIRING');
    }
    final result = await _post('/v1/pair', {'secret': ticket.secret});
    final token = result['token'];
    if (token is! String || !RegExp(r'^[0-9a-f]{64}$').hasMatch(token)) {
      close();
      throw const LoginAssistException('INVALID_PAIRING');
    }
    _token = token;
  }

  Future<Map<String, dynamic>> command(String op,
          [Map<String, dynamic> args = const {}]) =>
      _post('/v1/command', {'op': op, ...args});
  Future<Map<String, dynamic>> fill(int epoch, String field, String value) =>
      command('fill', {
        'epoch': epoch,
        'command': ++_command,
        'field': field,
        'value': value
      });
  Future<Map<String, dynamic>> click(
          int epoch, int x, int y, String revision) =>
      command('click', {
        'epoch': epoch,
        'command': ++_command,
        'x': x,
        'y': y,
        'revision': revision
      });
  Future<void> disconnect() async {
    try {
      if (!_closed && _token != null) {
        await command('cancel').timeout(const Duration(seconds: 1));
      }
    } catch (_) {
      // The host independently expires lost connections after ten seconds.
    } finally {
      close();
    }
  }

  void close() {
    _closed = true;
    _token = null;
    _client.close(force: true);
  }
}
