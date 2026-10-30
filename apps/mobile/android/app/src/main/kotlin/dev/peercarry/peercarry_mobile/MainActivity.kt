package dev.peercarry.peercarry_mobile

import io.flutter.embedding.android.FlutterActivity
import io.flutter.embedding.engine.FlutterEngine
import io.flutter.plugin.common.MethodChannel
import android.view.WindowManager

class MainActivity : FlutterActivity() {
    override fun configureFlutterEngine(flutterEngine: FlutterEngine) {
        super.configureFlutterEngine(flutterEngine)
        MethodChannel(flutterEngine.dartExecutor.binaryMessenger, "dev.peercarry/privacy")
            .setMethodCallHandler { call, result ->
                when (call.method) {
                    "enable" -> { window.addFlags(WindowManager.LayoutParams.FLAG_SECURE); result.success(null) }
                    "disable" -> { window.clearFlags(WindowManager.LayoutParams.FLAG_SECURE); result.success(null) }
                    else -> result.notImplemented()
                }
            }
    }
}
