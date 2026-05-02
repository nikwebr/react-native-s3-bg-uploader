package com.s3bguploader

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.Service
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.os.Build
import android.os.IBinder
import android.os.ParcelFileDescriptor
import androidx.core.app.NotificationCompat
import com.margelo.nitro.s3bguploader.GlobalUploaderState

class UploadForegroundService : Service() {

    private val pollerLock = Any()
    @Volatile private var pollerThread: Thread? = null
    @Volatile private var latestStartId: Int = 0

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onCreate() {
        super.onCreate()
        createNotificationChannel()
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        if (intent?.getStringExtra(EXTRA_TRANSFER_ID) == null) {
            stopSelf(startId)
            return START_NOT_STICKY
        }

        startForeground(NOTIFICATION_ID, buildNotification("S3 Upload", "Starting upload…", 0, indeterminate = true))
        latestStartId = startId

        synchronized(pollerLock) {
            val existing = pollerThread
            if (existing != null && existing.isAlive) {
                // A poller is already running and tracks session-level progress — no second thread needed.
                return START_NOT_STICKY
            }

            val thread = Thread {
                while (!Thread.interrupted()) {
                    try {
                        Thread.sleep(POLL_INTERVAL_MS)
                        // Poll session-level aggregate so all concurrent transfers are covered.
                        val aggJson = HybridS3BgUploader.nativeGetLiveAggregateProgressJson(null)
                        val agg = parseAggregateProgressForNotification(aggJson)
                        val isTerminal = agg.state == GlobalUploaderState.COMPLETED
                                || agg.state == GlobalUploaderState.FAILED
                        updateNotification(agg.percentage.toInt(), isTerminal)
                        if (isTerminal) break
                    } catch (_: InterruptedException) {
                        break
                    }
                }
                // Use latestStartId so the service only stops when no newer start has come in.
                stopSelf(latestStartId)
            }
            thread.isDaemon = true
            thread.start()
            pollerThread = thread
        }

        return START_NOT_STICKY
    }

    override fun onDestroy() {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            stopForeground(STOP_FOREGROUND_REMOVE)
        } else {
            @Suppress("DEPRECATION")
            stopForeground(true)
        }
        (getSystemService(NOTIFICATION_SERVICE) as NotificationManager).cancel(NOTIFICATION_ID)
        super.onDestroy()
    }

    private fun updateNotification(progressPct: Int, terminal: Boolean) {
        val nm = getSystemService(NOTIFICATION_SERVICE) as NotificationManager
        val title = HybridS3BgUploader.nativeGetFormattedTitle()
            .ifEmpty { "S3 Upload" }
        val subtitle = HybridS3BgUploader.nativeGetFormattedSubtitle()
            .ifEmpty { if (terminal) "Upload complete" else "Uploading… $progressPct%" }
        nm.notify(NOTIFICATION_ID, buildNotification(title, subtitle, progressPct,
            indeterminate = !terminal && progressPct <= 0))
    }

    private fun buildNotification(title: String, text: String, progressPct: Int, indeterminate: Boolean): Notification {
        return NotificationCompat.Builder(this, CHANNEL_ID)
            .setContentTitle(title)
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
                NotificationManager.IMPORTANCE_LOW,
            ).apply { description = "Background upload progress" }
            (getSystemService(NOTIFICATION_SERVICE) as NotificationManager)
                .createNotificationChannel(channel)
        }
    }

    companion object {
        const val CHANNEL_ID = "s3_upload_channel"
        const val NOTIFICATION_ID = 1001
        const val EXTRA_TRANSFER_ID = "transfer_id"
        private const val POLL_INTERVAL_MS = 500L

        fun openAsPfdStatic(context: Context, uri: String): ParcelFileDescriptor? {
            return try {
                if (uri.startsWith("content://")) {
                    context.contentResolver.openFileDescriptor(Uri.parse(uri), "r")
                } else {
                    val path = if (uri.startsWith("file://")) uri.removePrefix("file://") else uri
                    ParcelFileDescriptor.open(java.io.File(path), ParcelFileDescriptor.MODE_READ_ONLY)
                }
            } catch (e: Exception) {
                android.util.Log.e("S3Uploader", "openAsPfdStatic failed for $uri: $e")
                null
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Minimal JSON parse for notification percentage
// ---------------------------------------------------------------------------

private data class AggForNotification(
    val percentage: Double,
    val state: GlobalUploaderState,
)

private fun parseAggregateProgressForNotification(json: String): AggForNotification {
    return try {
        val o = org.json.JSONObject(json)
        AggForNotification(
            percentage = o.getDouble("percentage"),
            state = when (o.getString("state")) {
                "COMPLETED"     -> GlobalUploaderState.COMPLETED
                "FAILED"        -> GlobalUploaderState.FAILED
                "RUNNING_IN_BG" -> GlobalUploaderState.RUNNING_IN_BG
                "PAUSED"        -> GlobalUploaderState.PAUSED
                "RUNNING"       -> GlobalUploaderState.RUNNING
                else            -> GlobalUploaderState.NOT_STARTED
            },
        )
    } catch (_: Exception) {
        AggForNotification(0.0, GlobalUploaderState.NOT_STARTED)
    }
}
