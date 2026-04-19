package com.s3bguploader

import android.content.Intent
import android.os.Build
import com.margelo.nitro.s3bguploader.AggregateProgress
import com.margelo.nitro.s3bguploader.GlobalUploaderState
import com.margelo.nitro.s3bguploader.HybridS3BgUploaderSpec
import com.margelo.nitro.s3bguploader.UploadProgress
import com.margelo.nitro.s3bguploader.UploadState
import com.margelo.nitro.s3bguploader.Variant_NullType__fileProgress__UploadProgress__sessionAggregate__AggregateProgress__transferAggregate__AggregateProgress_____Unit
import org.json.JSONArray
import org.json.JSONObject

class HybridS3BgUploader : HybridS3BgUploaderSpec() {

    // -------------------------------------------------------------------------
    // Config
    // -------------------------------------------------------------------------

    override fun setConfig(
        startUploadApi: String,
        getUploadUrlsApi: String,
        completeApi: String,
    ): Unit {
        nativeSetConfig(startUploadApi, getUploadUrlsApi, completeApi)
    }

    // -------------------------------------------------------------------------
    // Progress callback
    // -------------------------------------------------------------------------

    override fun setProgressCallback(
        callback: Variant_NullType__fileProgress__UploadProgress__sessionAggregate__AggregateProgress__transferAggregate__AggregateProgress_____Unit?,
    ): Unit {
        UploadForegroundService.progressCallback = callback?.asSecondOrNull()
    }

    // -------------------------------------------------------------------------
    // Notification templates
    // -------------------------------------------------------------------------

    override fun setTaskTitle(title: String): Unit {
        appContext()?.let { StoragePathInit.ensureInit(it) }
        nativeSetTaskTitle(title)
    }

    override fun setTaskSubtitle(subtitle: String): Unit {
        appContext()?.let { StoragePathInit.ensureInit(it) }
        nativeSetTaskSubtitle(subtitle)
    }

    // -------------------------------------------------------------------------
    // Upload
    // -------------------------------------------------------------------------

    override fun uploadFile(
        filePath: String,
        transferId: String,
        userParams: Map<String, String>?,
    ): String {
        val context = appContext() ?: run {
            android.util.Log.e("S3Uploader", "No application context available")
            throw IllegalStateException("No application context")
        }

        // Set storage path on first call
        StoragePathInit.ensureInit(context)

        val paramsJson = if (!userParams.isNullOrEmpty()) {
            JSONObject(userParams as Map<*, *>).toString()
        } else {
            "{}"
        }

        val pfd = UploadForegroundService.openAsPfdStatic(context, filePath) ?: run {
            throw IllegalArgumentException("Cannot open file: $filePath")
        }
        val fd = pfd.detachFd()

        val fileKey = nativeUploadFile(fd, transferId, paramsJson)

        val intent = Intent(context, UploadForegroundService::class.java).apply {
            putExtra(UploadForegroundService.EXTRA_TRANSFER_ID, transferId)
        }
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            context.startForegroundService(intent)
        } else {
            context.startService(intent)
        }

        return fileKey
    }

    // -------------------------------------------------------------------------
    // Cancel / pause / resume
    // -------------------------------------------------------------------------

    override fun cancelFile(fileKey: String): Unit = nativeCancelFile(fileKey)

    override fun cancelTransfer(transferId: String): Unit = nativeCancelTransfer(transferId)

    override fun cancel(): Unit = nativeCancelAll()

    override fun pause(): Unit = nativePauseAll()

    override fun resume(): Unit = nativeResumeAll()

    // -------------------------------------------------------------------------
    // Progress queries
    // -------------------------------------------------------------------------

    override fun getProgress(
        transferId: String?,
        fileKey: String?,
    ): Array<UploadProgress> {
        val json = nativeGetProgressJson(transferId, fileKey)
        return parseUploadProgressArray(json)
    }

    override fun getAggregateProgress(transferId: String?): AggregateProgress {
        val json = nativeGetAggregateProgressJson(transferId)
        return parseAggregateProgress(json)
    }

    // -------------------------------------------------------------------------
    // Private helpers
    // -------------------------------------------------------------------------

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

    // -------------------------------------------------------------------------
    // JNI companion
    // -------------------------------------------------------------------------

    companion object {
        init {
            System.loadLibrary("uploader")
        }

        @JvmStatic external fun nativeSetConfig(
            startUploadApi: String,
            getUploadUrlsApi: String,
            completeApi: String,
        )

        @JvmStatic external fun nativeSetStoragePath(path: String)

        @JvmStatic external fun nativeSetTaskTitle(title: String)

        @JvmStatic external fun nativeSetTaskSubtitle(subtitle: String)

        /** Enqueues a file upload. Returns the S3 fileKey. Takes ownership of fd. */
        @JvmStatic external fun nativeUploadFile(
            fd: Int,
            transferId: String,
            userParamsJson: String,
        ): String

        @JvmStatic external fun nativeCancelFile(fileKey: String)
        @JvmStatic external fun nativeCancelTransfer(transferId: String)
        @JvmStatic external fun nativeCancelAll()
        @JvmStatic external fun nativePauseAll()
        @JvmStatic external fun nativeResumeAll()

        /** Returns JSON array string of UploadProgress objects. Nullable filters. */
        @JvmStatic external fun nativeGetProgressJson(
            transferId: String?,
            fileKey: String?,
        ): String

        /** Returns JSON object string of AggregateProgress. Nullable filter. */
        @JvmStatic external fun nativeGetAggregateProgressJson(transferId: String?): String

        /** Live per-file progress including in-flight bytes from ProgressManager. */
        @JvmStatic external fun nativeGetLiveProgressJson(
            transferId: String?,
            fileKey: String?,
        ): String

        /** Live aggregate progress including in-flight bytes from ProgressManager. */
        @JvmStatic external fun nativeGetLiveAggregateProgressJson(transferId: String?): String

        @JvmStatic external fun nativeGetFormattedTitle(): String
        @JvmStatic external fun nativeGetFormattedSubtitle(): String
    }
}

// ---------------------------------------------------------------------------
// JSON parsing helpers (internal so UploadForegroundService can reuse them)
// ---------------------------------------------------------------------------

internal fun parseUploadProgressArray(json: String): Array<UploadProgress> {
    return try {
        val arr = JSONArray(json)
        Array(arr.length()) { i ->
            val o = arr.getJSONObject(i)
            UploadProgress(
                fileKey        = o.getString("fileKey"),
                transferId     = o.getString("transferId"),
                totalBytes     = o.getLong("totalBytes").toDouble(),
                uploadedBytes  = o.getLong("uploadedBytes").toDouble(),
                completedParts = o.getInt("completedParts").toDouble(),
                totalParts     = o.getInt("totalParts").toDouble(),
                percentage     = o.getDouble("percentage"),
                state          = parseUploadState(o.getString("state")),
            )
        }
    } catch (e: Exception) {
        android.util.Log.e("S3Uploader", "parseUploadProgressArray failed: $e")
        emptyArray()
    }
}

internal fun parseAggregateProgress(json: String): AggregateProgress {
    return try {
        val o = JSONObject(json)
        AggregateProgress(
            percentage         = o.getDouble("percentage"),
            totalSize          = o.getLong("totalSize").toDouble(),
            uploadedSize       = o.getLong("uploadedSize").toDouble(),
            totalTransfers     = if (o.has("totalTransfers")) o.getInt("totalTransfers").toDouble() else null,
            completedTransfers = if (o.has("completedTransfers")) o.getInt("completedTransfers").toDouble() else null,
            totalFiles         = o.getInt("totalFiles").toDouble(),
            completedFiles     = o.getInt("completedFiles").toDouble(),
            state              = parseGlobalUploaderState(o.getString("state")),
        )
    } catch (e: Exception) {
        android.util.Log.e("S3Uploader", "parseAggregateProgress failed: $e")
        AggregateProgress(
            percentage = 0.0, totalSize = 0.0, uploadedSize = 0.0,
            totalTransfers = null, completedTransfers = null,
            totalFiles = 0.0, completedFiles = 0.0,
            state = GlobalUploaderState.NOT_STARTED,
        )
    }
}

private fun parseUploadState(s: String): UploadState = when (s) {
    "RUNNING"    -> UploadState.RUNNING
    "PAUSED"     -> UploadState.PAUSED
    "COMPLETED"  -> UploadState.COMPLETED
    "FAILED"     -> UploadState.FAILED
    else         -> UploadState.NOT_STARTED
}

private fun parseGlobalUploaderState(s: String): GlobalUploaderState = when (s) {
    "RUNNING"       -> GlobalUploaderState.RUNNING
    "RUNNING_IN_BG" -> GlobalUploaderState.RUNNING_IN_BG
    "PAUSED"        -> GlobalUploaderState.PAUSED
    "COMPLETED"     -> GlobalUploaderState.COMPLETED
    "FAILED"        -> GlobalUploaderState.FAILED
    else            -> GlobalUploaderState.NOT_STARTED
}

// ---------------------------------------------------------------------------
// Storage path — set once from app cache dir
// ---------------------------------------------------------------------------

private object StoragePathInit {
    @Volatile private var done = false

    fun ensureInit(context: android.content.Context) {
        if (done) return
        synchronized(this) {
            if (done) return
            val path = context.cacheDir.absolutePath
            HybridS3BgUploader.nativeSetStoragePath(path)
            done = true
        }
    }
}
