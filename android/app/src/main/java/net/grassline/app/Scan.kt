package net.grassline.app

import android.Manifest
import android.content.ActivityNotFoundException
import android.content.Intent
import android.content.pm.PackageManager
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.camera.core.CameraSelector
import androidx.camera.core.ImageAnalysis
import androidx.camera.core.ImageProxy
import androidx.camera.core.Preview
import androidx.camera.lifecycle.ProcessCameraProvider
import androidx.camera.view.PreviewView
import androidx.compose.foundation.layout.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import androidx.compose.ui.viewinterop.AndroidView
import androidx.core.content.ContextCompat
import androidx.lifecycle.compose.LocalLifecycleOwner
import com.google.mlkit.vision.barcode.common.Barcode
import com.google.mlkit.vision.codescanner.GmsBarcodeScannerOptions
import com.google.mlkit.vision.codescanner.GmsBarcodeScanning
import com.google.zxing.BarcodeFormat
import com.google.zxing.BinaryBitmap
import com.google.zxing.DecodeHintType
import com.google.zxing.MultiFormatReader
import com.google.zxing.PlanarYUVLuminanceSource
import com.google.zxing.ReaderException
import com.google.zxing.common.HybridBinarizer
import java.util.concurrent.Executors
import java.util.concurrent.atomic.AtomicBoolean

/**
 * Scans a QR code with what the phone has: an app that takes the ZXing scan
 * intent (Binary Eye, Barcode Scanner…), else Google's code scanner. When
 * neither is there, [onFallback] is called to show [QrScannerScreen].
 */
@Composable
fun rememberQrScan(onFallback: () -> Unit, onResult: (String) -> Unit): () -> Unit {
    val context = LocalContext.current
    val fallback by rememberUpdatedState(onFallback)
    val result by rememberUpdatedState(onResult)
    val zxing = rememberLauncherForActivityResult(ActivityResultContracts.StartActivityForResult()) { r ->
        r.data?.getStringExtra("SCAN_RESULT")?.let { result(it) }
    }
    val google = {
        try {
            val options = GmsBarcodeScannerOptions.Builder().setBarcodeFormats(Barcode.FORMAT_QR_CODE).build()
            GmsBarcodeScanning.getClient(context, options).startScan()
                .addOnSuccessListener { code -> code.rawValue?.let { result(it) } }
                // Not cancelled by the user: no Play services, or its module is not there yet.
                .addOnFailureListener { fallback() }
        } catch (e: Exception) {
            fallback()
        }
    }
    return {
        val intent = Intent("com.google.zxing.client.android.SCAN")
            .putExtra("SCAN_FORMATS", "QR_CODE")
            .putExtra("PROMPT_MESSAGE", "Scan the pairing code")
        if (intent.resolveActivity(context.packageManager) != null) {
            try {
                zxing.launch(intent)
            } catch (e: ActivityNotFoundException) {
                google()
            }
        } else {
            google()
        }
    }
}

/** Our own scanner: the camera, decoded with ZXing. */
@Composable
fun QrScannerScreen(onResult: (String) -> Unit, onCancel: () -> Unit) {
    val context = LocalContext.current
    var granted by remember {
        mutableStateOf(ContextCompat.checkSelfPermission(context, Manifest.permission.CAMERA) == PackageManager.PERMISSION_GRANTED)
    }
    var denied by remember { mutableStateOf(false) }
    val ask = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) {
        granted = it
        denied = !it
    }
    LaunchedEffect(Unit) { if (!granted) ask.launch(Manifest.permission.CAMERA) }
    Box(Modifier.fillMaxSize().systemBarsPadding()) {
        when {
            granted -> CameraPreview(onResult, Modifier.fillMaxSize())
            denied -> Text(
                "Grassline needs the camera to scan the code. You can paste it instead.",
                Modifier.align(Alignment.Center).padding(24.dp),
            )
        }
        Text(
            "Point the camera at the pairing code",
            Modifier.align(Alignment.TopCenter).padding(16.dp),
            style = MaterialTheme.typography.titleMedium,
        )
        Button(onCancel, Modifier.align(Alignment.BottomCenter).padding(24.dp)) { Text("Cancel") }
    }
}

@Composable
private fun CameraPreview(onResult: (String) -> Unit, modifier: Modifier) {
    val context = LocalContext.current
    val owner = LocalLifecycleOwner.current
    val result by rememberUpdatedState(onResult)
    val executor = remember { Executors.newSingleThreadExecutor() }
    val reader = remember {
        MultiFormatReader().apply {
            setHints(mapOf(DecodeHintType.POSSIBLE_FORMATS to listOf(BarcodeFormat.QR_CODE), DecodeHintType.TRY_HARDER to true))
        }
    }
    val found = remember { AtomicBoolean(false) }
    val view = remember { PreviewView(context) }
    DisposableEffect(owner) {
        val future = ProcessCameraProvider.getInstance(context)
        future.addListener({
            val provider = future.get()
            val preview = Preview.Builder().build().also { it.setSurfaceProvider(view.surfaceProvider) }
            val analysis = ImageAnalysis.Builder()
                .setBackpressureStrategy(ImageAnalysis.STRATEGY_KEEP_ONLY_LATEST)
                .build()
            analysis.setAnalyzer(executor) { image ->
                val text = image.use { decode(reader, it) }
                if (text != null && found.compareAndSet(false, true)) {
                    ContextCompat.getMainExecutor(context).execute { result(text) }
                }
            }
            provider.unbindAll()
            provider.bindToLifecycle(owner, CameraSelector.DEFAULT_BACK_CAMERA, preview, analysis)
        }, ContextCompat.getMainExecutor(context))
        onDispose {
            runCatching { future.get().unbindAll() }
            executor.shutdown()
        }
    }
    AndroidView({ view }, modifier)
}

/** The QR code in the luminance plane of a YUV frame, if there is one. */
private fun decode(reader: MultiFormatReader, image: ImageProxy): String? {
    val plane = image.planes[0]
    val buffer = plane.buffer
    // Rows may be padded to rowStride; the last one may not be.
    val bytes = ByteArray(plane.rowStride * image.height)
    buffer.get(bytes, 0, minOf(buffer.remaining(), bytes.size))
    val source = PlanarYUVLuminanceSource(bytes, plane.rowStride, image.height, 0, 0, image.width, image.height, false)
    return try {
        reader.decodeWithState(BinaryBitmap(HybridBinarizer(source))).text
    } catch (e: ReaderException) {
        null
    } finally {
        reader.reset()
    }
}
