package cc.attacca.zyris.mobile

import android.accessibilityservice.AccessibilityService
import android.app.Activity
import android.content.Context
import android.content.Intent
import android.graphics.Bitmap
import android.graphics.PixelFormat
import android.hardware.display.DisplayManager
import android.hardware.display.VirtualDisplay
import android.media.ImageReader
import android.media.projection.MediaProjection
import android.media.projection.MediaProjectionManager
import android.net.Uri
import android.os.Build
import android.os.Environment
import android.os.Handler
import android.os.HandlerThread
import android.provider.Settings
import android.util.Base64
import android.util.DisplayMetrics
import android.view.View
import android.view.WindowManager
import android.webkit.WebView
import androidx.core.view.ViewCompat
import androidx.core.view.WindowCompat
import androidx.core.view.WindowInsetsCompat
import androidx.activity.result.ActivityResult
import androidx.core.content.FileProvider
import app.tauri.annotation.ActivityCallback
import app.tauri.annotation.Command
import app.tauri.annotation.InvokeArg
import app.tauri.annotation.Permission
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin
import java.io.ByteArrayOutputStream
import java.io.File
import java.util.concurrent.Executors

@InvokeArg
class ShotArgs {
    var maxWidth: Int? = null
    var jpeg: Boolean = false
}

@InvokeArg
class PointArgs {
    var x: Float = 0f
    var y: Float = 0f
}

@InvokeArg
class StrokeArgs {
    var x1: Float = 0f
    var y1: Float = 0f
    var x2: Float = 0f
    var y2: Float = 0f
    var durationMs: Long = 300
}

@InvokeArg
class TextArgs {
    var text: String = ""
}

@InvokeArg
class KeyArgs {
    var name: String = ""
}

@InvokeArg
class PathArgs {
    var path: String = ""
}

@TauriPlugin(
    permissions = [
        Permission(strings = ["android.permission.POST_NOTIFICATIONS"], alias = "notifications"),
        Permission(strings = ["android.permission.RECORD_AUDIO"], alias = "microphone"),
    ]
)
class ZyrisPlugin(private val activity: Activity) : Plugin(activity) {
    /**
     * In the app's Rust library (`mobile.rs`): hands TLS verification the JVM and a Context, and
     * names this phone. An app's hostname on Android is always "localhost", which is what the
     * approval page showed until the phone's own name was passed down.
     */
    private external fun initTls(context: Context, deviceName: String)

    init {
        // Before the connection's first handshake, which the Rust side starts after plugins load.
        initTls(activity.applicationContext, deviceName())
    }

    /** The name the person gave the phone in its settings ("Galaxy S23"), else its model. */
    private fun deviceName(): String {
        val given = Settings.Global.getString(activity.contentResolver, "device_name")
        return if (given.isNullOrBlank()) Build.MODEL else given
    }

    /** Below the status bar: the app draws edge to edge, and its header sat under the clock. */
    override fun load(webView: WebView) {
        val content = activity.findViewById<View>(android.R.id.content)
        ViewCompat.setOnApplyWindowInsetsListener(content) { view, insets ->
            val top = insets.getInsets(WindowInsetsCompat.Type.systemBars() or WindowInsetsCompat.Type.displayCutout()).top
            view.setPadding(view.paddingLeft, top, view.paddingRight, view.paddingBottom)
            insets
        }
        content.setBackgroundColor(0xFF0F0C0A.toInt())
        // Light icons: the app is dark, and dark icons on it were nearly invisible.
        WindowCompat.getInsetsController(activity.window, content).isAppearanceLightStatusBars = false
        ViewCompat.requestApplyInsets(content)
    }

    /** Where every blocking answer is worked out, so the main thread stays free for callbacks. */
    private val worker = Executors.newSingleThreadExecutor()
    private val captureThread = HandlerThread("zyris-capture").apply { start() }
    private val captureHandler = Handler(captureThread.looper)

    private var projection: MediaProjection? = null
    private var virtualDisplay: VirtualDisplay? = null
    private var reader: ImageReader? = null

    /** The newest frame. Frames arrive only when the screen changes, so the last one is kept. */
    @Volatile
    private var latest: Bitmap? = null
    private var pendingShot: ShotArgs? = null

    private fun screenSize(): Triple<Int, Int, Int> {
        val metrics = DisplayMetrics()
        val window = activity.getSystemService(Context.WINDOW_SERVICE) as WindowManager
        return if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
            val bounds = window.currentWindowMetrics.bounds
            Triple(bounds.width(), bounds.height(), activity.resources.displayMetrics.densityDpi)
        } else {
            @Suppress("DEPRECATION")
            window.defaultDisplay.getRealMetrics(metrics)
            Triple(metrics.widthPixels, metrics.heightPixels, metrics.densityDpi)
        }
    }

    @Command
    fun display(invoke: Invoke) {
        val (width, height, dpi) = screenSize()
        invoke.resolve(JSObject().apply {
            put("width", width)
            put("height", height)
            put("scale", dpi / 160.0)
        })
    }

    // ---- The screen -------------------------------------------------------------------------

    @Command
    fun screenshot(invoke: Invoke) {
        val args = invoke.parseArgs(ShotArgs::class.java)
        if (projection != null) {
            worker.execute { shoot(invoke, args) }
            return
        }
        pendingShot = args
        val manager = activity.getSystemService(Context.MEDIA_PROJECTION_SERVICE) as MediaProjectionManager
        startActivityForResult(invoke, manager.createScreenCaptureIntent(), "projectionResult")
    }

    @ActivityCallback
    private fun projectionResult(invoke: Invoke, result: ActivityResult) {
        val data = result.data
        if (result.resultCode != Activity.RESULT_OK || data == null) {
            invoke.reject("The screen capture was not allowed on the phone.")
            return
        }
        ConnectionService.start(activity, projecting = true)
        worker.execute {
            // From Android 14 the token may only be redeemed once the service runs as a
            // media-projection foreground service, which it becomes asynchronously.
            Thread.sleep(500)
            try {
                val manager = activity.getSystemService(Context.MEDIA_PROJECTION_SERVICE) as MediaProjectionManager
                val granted = manager.getMediaProjection(result.resultCode, data)
                    ?: throw IllegalStateException("Android handed back no screen capture")
                startMirroring(granted)
                // The first frame takes a moment to arrive.
                var waited = 0
                while (latest == null && waited < 3000) {
                    Thread.sleep(50)
                    waited += 50
                }
                shoot(invoke, pendingShot ?: ShotArgs())
            } catch (error: Exception) {
                invoke.reject("The screen capture could not start: ${error.message}")
            }
        }
    }

    private fun startMirroring(granted: MediaProjection) {
        val (width, height, dpi) = screenSize()
        val imageReader = ImageReader.newInstance(width, height, PixelFormat.RGBA_8888, 2)
        imageReader.setOnImageAvailableListener({ source ->
            val image = source.acquireLatestImage() ?: return@setOnImageAvailableListener
            try {
                val plane = image.planes[0]
                val rowPixels = plane.rowStride / plane.pixelStride
                val padded = Bitmap.createBitmap(rowPixels, image.height, Bitmap.Config.ARGB_8888)
                padded.copyPixelsFromBuffer(plane.buffer)
                // ponytail: a full copy per changed frame; capture on demand if battery suffers.
                latest = Bitmap.createBitmap(padded, 0, 0, image.width, image.height)
            } finally {
                image.close()
            }
        }, captureHandler)
        granted.registerCallback(object : MediaProjection.Callback() {
            override fun onStop() {
                virtualDisplay?.release()
                reader?.close()
                virtualDisplay = null
                reader = null
                projection = null
                latest = null
            }
        }, captureHandler)
        virtualDisplay = granted.createVirtualDisplay(
            "zyris", width, height, dpi,
            DisplayManager.VIRTUAL_DISPLAY_FLAG_AUTO_MIRROR,
            imageReader.surface, null, captureHandler
        )
        reader = imageReader
        projection = granted
    }

    private fun shoot(invoke: Invoke, args: ShotArgs) {
        val frame = latest
        if (frame == null) {
            invoke.reject("No picture of the screen has arrived yet; try again in a moment.")
            return
        }
        val maxWidth = args.maxWidth
        val picture = if (maxWidth != null && maxWidth in 1 until frame.width) {
            Bitmap.createScaledBitmap(frame, maxWidth, frame.height * maxWidth / frame.width, true)
        } else {
            frame
        }
        val bytes = ByteArrayOutputStream()
        picture.compress(if (args.jpeg) Bitmap.CompressFormat.JPEG else Bitmap.CompressFormat.PNG, 85, bytes)
        invoke.resolve(JSObject().apply {
            put("data", Base64.encodeToString(bytes.toByteArray(), Base64.NO_WRAP))
            put("width", picture.width)
            put("height", picture.height)
            put("sourceWidth", frame.width)
            put("sourceHeight", frame.height)
        })
    }

    // ---- Touch and keys ---------------------------------------------------------------------

    private fun service(invoke: Invoke): ZyrisAccessibilityService? {
        val service = ZyrisAccessibilityService.instance
        if (service == null) {
            invoke.reject("Touch is off: turn on Zyris under Settings → Accessibility on the phone.")
        }
        return service
    }

    private fun answer(invoke: Invoke, done: Boolean, what: String) {
        if (done) invoke.resolve() else invoke.reject("The phone did not accept the $what.")
    }

    @Command
    fun tap(invoke: Invoke) {
        val args = invoke.parseArgs(PointArgs::class.java)
        val service = service(invoke) ?: return
        worker.execute { answer(invoke, service.stroke(args.x, args.y, args.x, args.y, 50), "tap") }
    }

    @Command
    fun swipe(invoke: Invoke) {
        val args = invoke.parseArgs(StrokeArgs::class.java)
        val service = service(invoke) ?: return
        worker.execute { answer(invoke, service.stroke(args.x1, args.y1, args.x2, args.y2, args.durationMs), "swipe") }
    }

    @Command
    fun typeText(invoke: Invoke) {
        val args = invoke.parseArgs(TextArgs::class.java)
        val service = service(invoke) ?: return
        answer(invoke, service.typeText(args.text), "text; is a text field focused?")
    }

    @Command
    fun key(invoke: Invoke) {
        val args = invoke.parseArgs(KeyArgs::class.java)
        val service = service(invoke) ?: return
        val done = when (args.name.lowercase()) {
            "back", "escape", "esc" -> service.global(AccessibilityService.GLOBAL_ACTION_BACK)
            "home" -> service.global(AccessibilityService.GLOBAL_ACTION_HOME)
            "recents", "overview", "alt+tab" -> service.global(AccessibilityService.GLOBAL_ACTION_RECENTS)
            "notifications" -> service.global(AccessibilityService.GLOBAL_ACTION_NOTIFICATIONS)
            "enter", "return" -> service.enter()
            else -> {
                invoke.reject("A phone has no \"${args.name}\" key. It knows Back, Home, Recents, Notifications and Enter.")
                return
            }
        }
        answer(invoke, done, "key ${args.name}")
    }

    // ---- What the person has allowed ---------------------------------------------------------

    @Command
    fun status(invoke: Invoke) {
        val allFiles = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) Environment.isExternalStorageManager() else true
        val installs = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) activity.packageManager.canRequestPackageInstalls() else true
        invoke.resolve(JSObject().apply {
            put("touch", ZyrisAccessibilityService.instance != null)
            put("screen", projection != null)
            put("allFiles", allFiles)
            put("installs", installs)
            put("storage", Environment.getExternalStorageDirectory().absolutePath)
        })
    }

    private fun openSettings(invoke: Invoke, action: String, withPackage: Boolean) {
        val intent = Intent(action)
        if (withPackage) intent.data = Uri.parse("package:${activity.packageName}")
        intent.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
        activity.startActivity(intent)
        invoke.resolve()
    }

    @Command
    fun openTouchSettings(invoke: Invoke) = openSettings(invoke, Settings.ACTION_ACCESSIBILITY_SETTINGS, false)

    @Command
    fun openFilesSettings(invoke: Invoke) {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
            openSettings(invoke, Settings.ACTION_MANAGE_APP_ALL_FILES_ACCESS_PERMISSION, true)
        } else {
            invoke.resolve()
        }
    }

    @Command
    fun startBackground(invoke: Invoke) {
        ConnectionService.start(activity, projecting = projection != null)
        invoke.resolve()
    }

    // ---- Updates ----------------------------------------------------------------------------

    /** Hand a downloaded APK to the system installer, which asks the person to confirm. */
    @Command
    fun installApk(invoke: Invoke) {
        val args = invoke.parseArgs(PathArgs::class.java)
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O && !activity.packageManager.canRequestPackageInstalls()) {
            openSettings(invoke, Settings.ACTION_MANAGE_UNKNOWN_APP_SOURCES, true)
            return
        }
        val uri = FileProvider.getUriForFile(activity, "${activity.packageName}.zyris.files", File(args.path))
        val intent = Intent(Intent.ACTION_VIEW)
            .setDataAndType(uri, "application/vnd.android.package-archive")
            .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_ACTIVITY_NEW_TASK)
        activity.startActivity(intent)
        invoke.resolve()
    }
}
