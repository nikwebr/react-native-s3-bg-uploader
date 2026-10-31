import UIKit
import React
import React_RCTAppDelegate
import ReactAppDependencyProvider
import CustomEncryptor

@main
class AppDelegate: UIResponder, UIApplicationDelegate {
  /// Filled in by SceneDelegate. The app adopts the UIScene lifecycle, which
  /// iOS 27 requires: UIKit traps at launch with
  /// "UIApplicationEvaluateRuntimeIssueForNoSceneLifecycleAdoption" otherwise.
  var window: UIWindow?

  var reactNativeDelegate: ReactNativeDelegate?
  var reactNativeFactory: RCTReactNativeFactory?

  func application(
    _ application: UIApplication,
    didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]? = nil
  ) -> Bool {
    // Register the custom AES-256-GCM encryptor factory before React starts.
    // No key lives here: each encryptAndUploadFile(...) call passes its own
    // keyHex/nonceHex, which the uploader hands to this scheme's create callback.
    CustomEncryptor.register()

    let delegate = ReactNativeDelegate()
    let factory = RCTReactNativeFactory(delegate: delegate)
    delegate.dependencyProvider = RCTAppDependencyProvider()

    reactNativeDelegate = delegate
    reactNativeFactory = factory

    return true
  }

  /// Starts React Native in the window owned by the app's scene.
  func startReactNative(in window: UIWindow, launchOptions: [UIApplication.LaunchOptionsKey: Any]?) {
    reactNativeFactory?.startReactNative(
      withModuleName: "CustomEncryptionExample",
      in: window,
      launchOptions: launchOptions
    )
  }
}

/// The app's single scene. The RN root view is created here instead of in
/// didFinishLaunching, because the scene (not the app delegate) owns the window
/// once the UIScene lifecycle is adopted.
class SceneDelegate: UIResponder, UIWindowSceneDelegate {
  var window: UIWindow?

  func scene(
    _ scene: UIScene,
    willConnectTo session: UISceneSession,
    options connectionOptions: UIScene.ConnectionOptions
  ) {
    guard let windowScene = scene as? UIWindowScene else { return }
    let window = UIWindow(windowScene: windowScene)
    self.window = window
    (UIApplication.shared.delegate as? AppDelegate)?.startReactNative(
      in: window,
      launchOptions: nil
    )
  }
}

class ReactNativeDelegate: RCTDefaultReactNativeFactoryDelegate {
  override func sourceURL(for bridge: RCTBridge) -> URL? {
    self.bundleURL()
  }

  override func bundleURL() -> URL? {
#if DEBUG
    RCTBundleURLProvider.sharedSettings().jsBundleURL(forBundleRoot: "index")
#else
    Bundle.main.url(forResource: "main", withExtension: "jsbundle")
#endif
  }
}
