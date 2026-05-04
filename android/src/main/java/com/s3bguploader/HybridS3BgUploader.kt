package com.s3bguploader

import android.content.Intent
import android.os.Build
import com.margelo.nitro.core.Promise
import com.margelo.nitro.s3bguploader.AggregateProgress
import com.margelo.nitro.s3bguploader.GlobalUploaderState
import com.margelo.nitro.s3bguploader.HybridS3BgUploaderSpec
import com.margelo.nitro.s3bguploader.UploadProgress
import com.margelo.nitro.s3bguploader.UploadState
import com.margelo.nitro.s3bguploader.Variant_NullType__fileProgress__UploadProgress__sessionAggregate__AggregateProgress__transferAggregate__AggregateProgress_____Unit
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.asCoroutineDispatcher
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Semaphore
import kotlinx.coroutines.sync.withPermit
import org.json.JSONArray
import org.json.JSONObject
import java.util.concurrent.Executors

class HybridS3BgUploader : HybridS3BgUploaderSpec() {

    // Serial queue: max 1 concurrent hash
    private val hashDispatcher = Executors.newSingleThreadExecutor().asCoroutineDispatcher()
    private val hashScope = CoroutineScope(hashDispatcher + SupervisorJob())

    // Concurrent queue with semaphore: max 2 concurrent start_api calls
    private val initApiSemaphore = Semaphore(2)
    private val initApiScope = CoroutineScope(Dispatchers.IO + SupervisorJob())

    // Files waiting for resume() to trigger start_api
    private data class PendingFile(val hash: String, val transferId: String, val filePath: String)
    private val pendingFiles = mutableListOf<PendingFile>()

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
        progressCallback = callback?.asSecondOrNull()
        nativeInitProgressCallback()
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
    ): Promise<String> {
        return Promise.async(hashScope) {
            val context = appContext() ?: throw IllegalStateException("No application context")
            StoragePathInit.ensureInit(context)

            val paramsJson = if (!userParams.isNullOrEmpty()) {
                JSONObject(userParams as Map<*, *>).toString()
            } else {
                "{}"
            }

            val pfd = UploadForegroundService.openAsPfdStatic(context, filePath)
                ?: throw IllegalArgumentException("Cannot open file: $filePath")
            val fd = pfd.detachFd()
            val fileName = resolveFileName(context, filePath)

            val rawResult = nativeHashAndPreRegister(fd, fileName, transferId, paramsJson)
            if (rawResult.isEmpty()) throw RuntimeException("hash_and_pre_register failed for $filePath")
            if (rawResult.startsWith("ERROR:")) throw RuntimeException(rawResult.removePrefix("ERROR:"))
            val fileHash = rawResult

            // If already running, trigger start_api eagerly; otherwise defer until resume().
            val globalState = nativeGetGlobalState()
            if (globalState == "RUNNING" || globalState == "RUNNING_IN_BG") {
                launchInitApi(fileHash, transferId, filePath)
            } else {
                synchronized(pendingFiles) {
                    pendingFiles.add(PendingFile(fileHash, transferId, filePath))
                }
            }

            fileHash
        }
    }

    private fun launchInitApi(hash: String, transferId: String, filePath: String) {
        initApiScope.launch {
            initApiSemaphore.withPermit {
                val context = appContext() ?: return@withPermit
                val pfd = UploadForegroundService.openAsPfdStatic(context, filePath)
                    ?: return@withPermit
                val fd = pfd.detachFd()
                nativeInitializeFile(fd, hash, transferId)
            }
        }
    }

    private fun resolveFileName(context: android.content.Context, uriString: String): String {
        val uri = android.net.Uri.parse(uriString)
        if (uri.scheme == "content") {
            context.contentResolver.query(
                uri,
                arrayOf(android.provider.OpenableColumns.DISPLAY_NAME),
                null, null, null,
            )?.use { cursor ->
                if (cursor.moveToFirst()) {
                    val idx = cursor.getColumnIndex(android.provider.OpenableColumns.DISPLAY_NAME)
                    if (idx >= 0) {
                        val name = cursor.getString(idx)
                        if (!name.isNullOrEmpty()) return name
                    }
                }
            }
        }
        return uri.lastPathSegment?.substringAfterLast('/') ?: "file"
    }

    // -------------------------------------------------------------------------
    // Cancel / pause / resume
    // -------------------------------------------------------------------------

    override fun cancelFile(fileHash: String): Unit = nativeCancelFile(fileHash)

    override fun cancelTransfer(transferId: String): Unit = nativeCancelTransfer(transferId)

    override fun cancel(): Unit {
        synchronized(pendingFiles) { pendingFiles.clear() }
        nativeCancelAll()
        stopForegroundService()
    }

    override fun pause(): Unit {
        nativePauseAll()
        stopForegroundService()
    }

    override fun resume(): Promise<Unit> {
        return Promise.async {
            val err = nativeResumeAll()
            if (err != null) throw Exception(err)
            val toProcess = synchronized(pendingFiles) { pendingFiles.toList().also { pendingFiles.clear() } }
            for (p in toProcess) {
                launchInitApi(p.hash, p.transferId, p.filePath)
            }
            appContext()?.let { ctx ->
                val intent = Intent(ctx, UploadForegroundService::class.java).apply {
                    putExtra(UploadForegroundService.EXTRA_TRANSFER_ID, "session")
                }
                if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
                    ctx.startForegroundService(intent)
                } else {
                    ctx.startService(intent)
                }
            }
        }
    }

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

    private fun stopForegroundService() {
        appContext()?.let { ctx ->
            ctx.startService(Intent(ctx, UploadForegroundService::class.java).apply {
                action = UploadForegroundService.ACTION_STOP
            })
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

    // -------------------------------------------------------------------------
    // JNI companion
    // -------------------------------------------------------------------------

    companion object {
        init {
            System.loadLibrary("uploader")
        }

        @Volatile var progressCallback: ((UploadProgress, AggregateProgress, AggregateProgress) -> Unit)? = null

        private val mainHandler by lazy { android.os.Handler(android.os.Looper.getMainLooper()) }

        /** Called from Rust on a background thread for every progress event. */
        @JvmStatic
        fun onNativeProgress(json: String) {
            val cb = progressCallback ?: return
            try {
                val o = org.json.JSONObject(json)
                val fp = parseUploadProgressFromObject(o.getJSONObject("file"))
                val sessionAgg = parseAggregateProgress(o.getJSONObject("sessionAgg").toString())
                val transferAgg = parseAggregateProgress(o.getJSONObject("transferAgg").toString())
                mainHandler.post { cb(fp, sessionAgg, transferAgg) }
            } catch (e: Exception) {
                android.util.Log.e("S3Uploader", "onNativeProgress parse error: $e")
            }
        }

        private fun parseUploadProgressFromObject(o: org.json.JSONObject): UploadProgress {
            return UploadProgress(
                fileKey        = if (o.has("fileKey")) o.getString("fileKey") else null,
                fileName       = o.optString("fileName", ""),
                fileHash       = o.optString("fileHash", ""),
                transferId     = o.getString("transferId"),
                totalBytes     = o.getLong("totalBytes").toDouble(),
                uploadedBytes  = o.getLong("uploadedBytes").toDouble(),
                completedParts = o.getInt("completedParts").toDouble(),
                totalParts     = o.getInt("totalParts").toDouble(),
                percentage     = o.getDouble("percentage"),
                state          = parseUploadState(o.getString("state")),
            )
        }

        @JvmStatic external fun nativeSetConfig(
            startUploadApi: String,
            getUploadUrlsApi: String,
            completeApi: String,
        )

        @JvmStatic external fun nativeSetStoragePath(path: String)

        @JvmStatic external fun nativeSetTaskTitle(title: String)

        @JvmStatic external fun nativeSetTaskSubtitle(subtitle: String)

        /** Phase 1: hash fd and pre-register. Returns file hash. Takes ownership of fd. */
        @JvmStatic external fun nativeHashAndPreRegister(
            fd: Int,
            fileName: String,
            transferId: String,
            userParamsJson: String,
        ): String

        /** Phase 2: call start_api and enqueue the file. Returns file key. Takes ownership of fd. */
        @JvmStatic external fun nativeInitializeFile(
            fd: Int,
            fileHash: String,
            transferId: String,
        ): String

        @JvmStatic external fun nativeGetGlobalState(): String

        @JvmStatic external fun nativeCancelFile(fileHash: String)
        @JvmStatic external fun nativeCancelTransfer(transferId: String)
        @JvmStatic external fun nativeCancelAll()
        @JvmStatic external fun nativePauseAll()
        @JvmStatic external fun nativeResumeAll(): String?

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
        @JvmStatic external fun nativeInitProgressCallback()
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
                fileKey        = if (o.has("fileKey")) o.getString("fileKey") else null,
                fileName       = o.optString("fileName", ""),
                fileHash       = o.optString("fileHash", ""),
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
    "INITIALIZED" -> UploadState.INITIALIZED
    "RUNNING"     -> UploadState.RUNNING
    "PAUSED"      -> UploadState.PAUSED
    "COMPLETED"   -> UploadState.COMPLETED
    "FAILED"      -> UploadState.FAILED
    "CANCELLED"   -> UploadState.CANCELLED
    else          -> UploadState.NOT_STARTED
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
