package com.s3bguploader

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.Service
import android.content.Intent
import android.os.Build
import android.os.IBinder
import android.os.ParcelFileDescriptor
import androidx.core.app.NotificationCompat
import com.margelo.nitro.s3bguploader.UploadProgress
import com.margelo.nitro.s3bguploader.UploadState

class UploadForegroundService : Service() {

    private val mainHandler = android.os.Handler(android.os.Looper.getMainLooper())
    private val progressLock = Any()
    private var lastProgressMs = 0L
    private var lastPercentage = -1.0

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onCreate() {
        super.onCreate()
        createNotificationChannel()
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        val filePath = intent?.getStringExtra(EXTRA_FILE_PATH) ?: run {
            stopSelf()
            return START_NOT_STICKY
        }

        startForeground(NOTIFICATION_ID, buildNotification("Upload wird gestartet…", 0, indeterminate = true))

        Thread {
            val pfd = openAsPfd(filePath)
            if (pfd == null) {
                android.util.Log.e("S3Uploader", "openAsPfd returned null for: $filePath")
                stopSelf(startId)
                return@Thread
            }
            val fd = pfd.detachFd()
            HybridS3BgUploader.nativeUploadFileStatic(fd, object : HybridS3BgUploader.ProgressListener {
                override fun onProgress(
                    totalBytes: Long,
                    uploadedBytes: Long,
                    completedParts: Int,
                    totalParts: Int,
                    percentage: Double,
                    state: String
                ) {
                    val uploadState = when (state) {
                        "RUNNING"  -> UploadState.RUNNING
                        "PAUSED"   -> UploadState.PAUSED
                        "FINISHED" -> UploadState.FINISHED
                        else       -> UploadState.FAILED
                    }
                    val progress = UploadProgress(
                        totalBytes = totalBytes.toDouble(),
                        uploadedBytes = uploadedBytes.toDouble(),
                        completedParts = completedParts.toDouble(),
                        totalParts = totalParts.toDouble(),
                        percentage = percentage,
                        state = uploadState
                    )

                    val isTerminal = uploadState == UploadState.FINISHED || uploadState == UploadState.FAILED
                    val now = System.currentTimeMillis()

                    val shouldUpdate = synchronized(progressLock) {
                        when {
                            isTerminal -> true
                            percentage > lastPercentage -> {
                                lastPercentage = percentage
                                lastProgressMs = now
                                true
                            }
                            // Zeit-Throttle: nur durchlassen wenn percentage nicht rückwärts geht
                            percentage >= lastPercentage && (now - lastProgressMs) >= THROTTLE_MS -> {
                                lastProgressMs = now
                                true
                            }
                            else -> false
                        }
                    }

                    if (!shouldUpdate) return

                    val label = when (uploadState) {
                        UploadState.FINISHED -> "Upload abgeschlossen"
                        UploadState.FAILED   -> "Upload fehlgeschlagen"
                        else -> "Hochladen… ${percentage.toInt()}%"
                    }
                    updateNotification(label, percentage.toInt(), indeterminate = uploadState == UploadState.RUNNING && percentage <= 0)

                    mainHandler.post {
                        progressCallback?.invoke(progress)
                    }

                    if (isTerminal) {
                        stopSelf(startId)
                    }
                }
            })
        }.start()

        return START_NOT_STICKY
    }

    override fun onTaskRemoved(rootIntent: Intent?) {
        super.onTaskRemoved(rootIntent)
    }

    private fun updateNotification(text: String, progressPct: Int, indeterminate: Boolean) {
        val nm = getSystemService(NOTIFICATION_SERVICE) as NotificationManager
        nm.notify(NOTIFICATION_ID, buildNotification(text, progressPct, indeterminate))
    }

    private fun buildNotification(text: String, progressPct: Int, indeterminate: Boolean): Notification {
        return NotificationCompat.Builder(this, CHANNEL_ID)
            .setContentTitle("Datei-Upload")
            .setContentText(text)
            .setSmallIcon(android.R.drawable.stat_sys_upload)
            .setOngoing(true)
            .setOnlyAlertOnce(true)
            .setProgress(100, progressPct, indeterminate)
            .build()
    }

    private fun createNotificationChannel() {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            val channel = NotificationChannel(
                CHANNEL_ID,
                "S3 Upload",
                NotificationManager.IMPORTANCE_LOW
            ).apply { description = "Fortschritt laufender Uploads" }
            (getSystemService(NOTIFICATION_SERVICE) as NotificationManager)
                .createNotificationChannel(channel)
        }
    }

    private fun openAsPfd(uri: String): ParcelFileDescriptor? {
        return try {
            if (uri.startsWith("content://")) {
                contentResolver.openFileDescriptor(android.net.Uri.parse(uri), "r")
            } else {
                val path = if (uri.startsWith("file://")) uri.removePrefix("file://") else uri
                ParcelFileDescriptor.open(java.io.File(path), ParcelFileDescriptor.MODE_READ_ONLY)
            }
        } catch (e: Exception) {
            android.util.Log.e("S3Uploader", "openAsPfd failed for $uri: $e")
            null
        }
    }

    companion object {
        const val CHANNEL_ID = "s3_upload_channel"
        const val NOTIFICATION_ID = 1001
        const val EXTRA_FILE_PATH = "file_path"
        private const val THROTTLE_MS = 100L

        // Statischer Callback — wird von HybridS3BgUploader gesetzt
        @Volatile var progressCallback: ((UploadProgress) -> Unit)? = null
    }
}
