package com.s3bguploader

import android.content.Intent
import android.os.Build
import com.margelo.nitro.s3bguploader.HybridS3BgUploaderSpec
import com.margelo.nitro.s3bguploader.UploadProgress
import com.margelo.nitro.s3bguploader.Variant_NullType__progress__UploadProgress_____Unit

class HybridS3BgUploader : HybridS3BgUploaderSpec() {

    private var progressCallback: ((UploadProgress) -> Unit)? = null

    override fun sum(num1: Double, num2: Double): Double {
        return num1 + num2
    }

    override fun setProgressCallback(callback: Variant_NullType__progress__UploadProgress_____Unit?): Unit {
        progressCallback = callback?.asSecondOrNull()
    }

    override fun uploadFile(filePath: String): Unit {
        val context = appContext() ?: run {
            android.util.Log.e("S3Uploader", "No application context available")
            return
        }

        UploadForegroundService.progressCallback = progressCallback

        val intent = Intent(context, UploadForegroundService::class.java).apply {
            putExtra(UploadForegroundService.EXTRA_FILE_PATH, filePath)
        }
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            context.startForegroundService(intent)
        } else {
            context.startService(intent)
        }
    }

    private fun appContext(): android.content.Context? {
        S3BgUploaderPackage.appContext?.let { return it }
        return try {
            val cls = Class.forName("android.app.ActivityThread")
            val app = cls.getMethod("currentApplication").invoke(null)
                    as? android.content.Context
            if (app != null) S3BgUploaderPackage.appContext = app
            app
        } catch (_: Exception) { null }
    }

    interface ProgressListener {
        fun onProgress(
            totalBytes: Long,
            uploadedBytes: Long,
            completedParts: Int,
            totalParts: Int,
            percentage: Double,
            state: String
        )
    }

    companion object {
        init {
            System.loadLibrary("uploader")
        }

        @JvmStatic
        external fun nativeUploadFileStatic(fd: Int, callback: ProgressListener): Int
    }
}
