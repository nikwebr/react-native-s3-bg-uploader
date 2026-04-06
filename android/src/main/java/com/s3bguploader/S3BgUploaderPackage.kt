package com.s3bguploader;

import com.facebook.react.bridge.NativeModule;
import com.facebook.react.bridge.ReactApplicationContext;
import com.facebook.react.module.model.ReactModuleInfoProvider;
import com.facebook.react.TurboReactPackage;
import com.margelo.nitro.s3bguploader.S3BgUploaderOnLoad;


public class S3BgUploaderPackage : TurboReactPackage() {
  override fun getModule(name: String, reactContext: ReactApplicationContext): NativeModule? {
    appContext = reactContext.applicationContext
    return null
  }

  override fun getReactModuleInfoProvider(): ReactModuleInfoProvider = ReactModuleInfoProvider { emptyMap() }

  override fun createNativeModules(reactContext: ReactApplicationContext): MutableList<NativeModule> {
    appContext = reactContext.applicationContext
    return mutableListOf()
  }

  companion object {
    @Volatile var appContext: android.content.Context? = null

    init {
      S3BgUploaderOnLoad.initializeNative();
    }
  }
}
