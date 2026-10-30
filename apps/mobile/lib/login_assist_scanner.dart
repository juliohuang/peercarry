import 'dart:async';
import 'package:flutter/material.dart';
import 'package:mobile_scanner/mobile_scanner.dart';
import 'login_assist_client.dart';
import 'app_theme.dart';
import 'connection_client.dart';
import 'app_localizations.dart';

class LoginAssistScanner extends StatefulWidget {
  const LoginAssistScanner({super.key, this.forTransfer = false});
  final bool forTransfer;
  @override
  State<LoginAssistScanner> createState() => _LoginAssistScannerState();
}

class _LoginAssistScannerState extends State<LoginAssistScanner>
    with WidgetsBindingObserver {
  final controller = MobileScannerController(
      formats: [BarcodeFormat.qrCode], returnImage: false);
  bool done = false;
  AppLocalizations get l10n => AppLocalizations.of(context);
  @override
  void initState() {
    super.initState();
    WidgetsBinding.instance.addObserver(this);
  }

  @override
  void didChangeAppLifecycleState(AppLifecycleState state) {
    if (!controller.value.hasCameraPermission) return;
    if (state == AppLifecycleState.resumed && !done) {
      unawaited(controller.start().catchError((_) {}));
    } else {
      unawaited(controller.stop().catchError((_) {}));
    }
  }

  @override
  void dispose() {
    WidgetsBinding.instance.removeObserver(this);
    unawaited(controller.dispose());
    super.dispose();
  }

  @override
  Widget build(BuildContext context) => Scaffold(
        backgroundColor: CarryColors.background,
        appBar: AppBar(title: Text(l10n.text('scanComputerPairing'))),
        body: SafeArea(
          child: Stack(children: [
            MobileScanner(
                controller: controller,
                onDetect: (capture) {
                  if (done) return;
                  for (final barcode in capture.barcodes) {
                    final value = barcode.rawValue;
                    if (value == null) continue;
                    try {
                      if (widget.forTransfer) {
                        ConnectionTicket.parse(value);
                      } else {
                        LoginTicket.parse(value);
                      }
                    } catch (_) {
                      continue;
                    }
                    done = true;
                    unawaited(controller.stop());
                    Navigator.pop(context, value);
                    break;
                  }
                },
                errorBuilder: (context, error) {
                  final l10n = AppLocalizations.of(context);
                  return Center(
                    child:
                        CarryNotice(text: l10n.text('noCamera'), isError: true),
                  );
                }),
            IgnorePointer(
              child: Align(
                alignment: Alignment.bottomCenter,
                child: Padding(
                  padding: const EdgeInsets.all(20),
                  child: CarryCard(
                    child: Row(children: [
                      const Icon(Icons.info_outline, color: CarryColors.forest),
                      const SizedBox(width: 10),
                      Expanded(child: Text(l10n.text('scanHint'))),
                    ]),
                  ),
                ),
              ),
            ),
          ]),
        ),
      );
}
