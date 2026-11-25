package app.rufin.rufin

import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.content.ServiceConnection
import android.content.pm.ServiceInfo
import android.os.Build
import android.os.IBinder
import android.util.Log
import androidx.core.app.NotificationCompat
import androidx.core.app.ServiceCompat
import androidx.core.content.ContextCompat
import app.rufin.rufin.core.translate

/** Keeps Rufin running while none of its screens shows: Android freezes an app in the background and cuts its
 *  network unless a foreground service runs. Holding the playback service keeps the shared runtime, and with it
 *  Rufin Connect, open; the playback service starts and stops this one as "Keep running in the background" says. */
@androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)
class BackgroundService : Service() {
    private val runtime = object : ServiceConnection {
        override fun onServiceConnected(name: ComponentName?, service: IBinder?) = Unit
        override fun onServiceDisconnected(name: ComponentName?) = Unit
    }

    override fun onCreate() {
        super.onCreate()
        bindService(Intent(this, RufinService::class.java).setAction(RufinService.ACTION_BIND_RUNTIME), runtime, BIND_AUTO_CREATE)
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        if (Build.VERSION.SDK_INT >= 26) getSystemService(NotificationManager::class.java).createNotificationChannel(
            NotificationChannel(CHANNEL, translate("Background"), NotificationManager.IMPORTANCE_MIN))
        val notification = NotificationCompat.Builder(this, CHANNEL)
            .setSmallIcon(R.drawable.rufin_cover_fallback_symbolic)
            .setContentTitle(getString(R.string.app_name))
            .setContentText(translate("Running in the background"))
            .setContentIntent(PendingIntent.getActivity(this, 0, Intent(this, RufinActivity::class.java), PendingIntent.FLAG_IMMUTABLE))
            .setCategory(NotificationCompat.CATEGORY_SERVICE)
            .setPriority(NotificationCompat.PRIORITY_MIN)
            .setOngoing(true)
            .build()
        try {
            ServiceCompat.startForeground(this, NOTIFICATION, notification, ServiceInfo.FOREGROUND_SERVICE_TYPE_CONNECTED_DEVICE)
        } catch (refused: IllegalStateException) {
            // Android restarts a stopped service in the background, where it may refuse the foreground.
            Log.w("Rufin", "Android did not let Rufin keep running in the background", refused)
            stopSelf()
            return START_NOT_STICKY
        }
        return START_STICKY
    }

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onDestroy() {
        unbindService(runtime)
        super.onDestroy()
    }

    companion object {
        private const val CHANNEL = "background"
        private const val NOTIFICATION = 1002

        /** Whether it runs now. Android refuses the start while none of Rufin's screens shows, unless its battery
         *  use is unrestricted. */
        fun set(context: Context, running: Boolean): Boolean {
            val intent = Intent(context, BackgroundService::class.java)
            if (!running) { context.stopService(intent); return false }
            return try { ContextCompat.startForegroundService(context, intent); true }
            catch (refused: IllegalStateException) { Log.w("Rufin", "Android did not let Rufin keep running in the background", refused); false }
        }
    }
}
