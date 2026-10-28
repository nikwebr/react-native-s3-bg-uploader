package com.customencryptionexample

import android.app.Application
import com.facebook.react.PackageList
import com.facebook.react.ReactApplication
import com.facebook.react.ReactHost
import com.facebook.react.ReactNativeApplicationEntryPoint.loadReactNative
import com.facebook.react.defaults.DefaultReactHost.getDefaultReactHost
import com.s3bguploader.HybridS3BgUploader

class MainApplication : Application(), ReactApplication {

  override val reactHost: ReactHost by lazy {
    getDefaultReactHost(
      context = applicationContext,
      packageList =
        PackageList(this).packages.apply {
          // Packages that cannot be autolinked yet can be added manually here, for example:
          // add(MyReactNativePackage())
        },
    )
  }

  override fun onCreate() {
    super.onCreate()
    // Register the custom encryptor after libS3BgUploader.so is loaded.
    // This must happen in onCreate() (not in a companion init block) so that
    // libcustom_encryptor.so shares the same class loader namespace as
    // libuploader.so, ensuring the Rust CUSTOM_ENCRYPTOR static is shared.
    HybridS3BgUploader.registerCustomEncryptorIfAvailable()
    loadReactNative(this)
  }
}
