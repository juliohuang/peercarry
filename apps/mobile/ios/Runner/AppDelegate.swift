import Flutter
import UIKit

@main
@objc class AppDelegate: FlutterAppDelegate {
  private var privateLogin = false
  private var privacyCover: UIView?
  override func application(
    _ application: UIApplication,
    didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]?
  ) -> Bool {
    GeneratedPluginRegistrant.register(with: self)
    if let controller = window?.rootViewController as? FlutterViewController {
      FlutterMethodChannel(name: "dev.peercarry/privacy", binaryMessenger: controller.binaryMessenger)
        .setMethodCallHandler { [weak self] call, result in
          switch call.method {
          case "enable": self?.privateLogin = true; result(nil)
          case "disable": self?.privateLogin = false; result(nil)
          default: result(FlutterMethodNotImplemented)
          }
        }
    }
    return super.application(application, didFinishLaunchingWithOptions: launchOptions)
  }

  override func applicationWillResignActive(_ application: UIApplication) {
    if privateLogin, let window = window {
      let cover = UIView(frame: window.bounds)
      cover.backgroundColor = .systemBackground
      cover.autoresizingMask = [.flexibleWidth, .flexibleHeight]
      window.addSubview(cover)
      privacyCover = cover
    }
    super.applicationWillResignActive(application)
  }

  override func applicationDidBecomeActive(_ application: UIApplication) {
    privacyCover?.removeFromSuperview()
    privacyCover = nil
    super.applicationDidBecomeActive(application)
  }
}
