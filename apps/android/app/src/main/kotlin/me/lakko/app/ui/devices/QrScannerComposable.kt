// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.devices

import android.annotation.SuppressLint
import android.util.Log
import androidx.camera.core.CameraSelector
import androidx.camera.core.ExperimentalGetImage
import androidx.camera.core.ImageAnalysis
import androidx.camera.core.ImageProxy
import androidx.camera.core.Preview
import androidx.camera.lifecycle.ProcessCameraProvider
import androidx.camera.view.PreviewView
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.remember
import androidx.compose.ui.Modifier
import androidx.compose.ui.viewinterop.AndroidView
import androidx.core.content.ContextCompat
import androidx.lifecycle.findViewTreeLifecycleOwner
import com.google.mlkit.vision.barcode.BarcodeScanner
import com.google.mlkit.vision.barcode.BarcodeScannerOptions
import com.google.mlkit.vision.barcode.BarcodeScanning
import com.google.mlkit.vision.barcode.common.Barcode
import com.google.mlkit.vision.common.InputImage
import java.util.concurrent.Executors
import java.util.concurrent.atomic.AtomicBoolean

/**
 * Compose camera + QR scanner (Phase 2a3b4, mirror of
 * `apps/ios/Lakko/UI/Devices/QrScannerView.swift`).
 *
 * Three pieces wired together:
 *
 *   1. **CameraX** owns the device camera. `ProcessCameraProvider`
 *      binds a `Preview` (live frames into a [PreviewView]) +
 *      an `ImageAnalysis` (one frame at a time into our analyser)
 *      against the lifecycle of the host Composable. CameraX
 *      handles permission denial, surface rotation, and the
 *      tear-down dance on screen-off.
 *
 *   2. **ML Kit barcode-scanning** runs against each
 *      `ImageProxy` from the analyser. We constrain the
 *      decoder to QR only (`FORMAT_QR_CODE`) so the SDK doesn't
 *      waste cycles on Code128 / EAN / DataMatrix.
 *
 *   3. **`hasDetected` latch** — same shape as the iOS
 *      `ScannerViewController`. Once we fire [onDetected] for
 *      the first match we stop dispatching subsequent frames to
 *      the callback so SwiftUI / Compose can tear the view down
 *      without us racing on a second "detected" call.
 *
 * The caller assumes the runtime CAMERA permission has been
 * granted before this composable is mounted —
 * [DevicesScreen] gates the mount on the runtime permission
 * grant.
 */
@OptIn(ExperimentalGetImage::class)
@Composable
public fun QrScannerComposable(
    onDetected: (String) -> Unit,
    onError: (String) -> Unit,
    modifier: Modifier = Modifier,
) {
    val hasDetected = remember { AtomicBoolean(false) }

    // Dedicated background executor for the analyser — CameraX
    // requires a non-main executor for ImageAnalysis or it
    // blocks the UI thread on every YUV frame.
    val analysisExecutor = remember { Executors.newSingleThreadExecutor() }
    val scanner: BarcodeScanner = remember {
        BarcodeScanning.getClient(
            BarcodeScannerOptions.Builder()
                .setBarcodeFormats(Barcode.FORMAT_QR_CODE)
                .build(),
        )
    }

    DisposableEffect(Unit) {
        onDispose {
            analysisExecutor.shutdown()
            try {
                scanner.close()
            } catch (_: Throwable) {
                // best-effort
            }
        }
    }

    AndroidView(
        modifier = modifier.fillMaxSize(),
        factory = { ctx ->
            val previewView = PreviewView(ctx).apply {
                scaleType = PreviewView.ScaleType.FILL_CENTER
            }
            previewView
        },
        update = { previewView ->
            // The factory ran once; the update block fires on
            // every Compose recomposition. Binding the camera
            // provider inside `update` lets us re-bind cleanly
            // if the host's `lifecycleOwner` ever changes (rare
            // — almost always the Activity).
            bindCamera(
                previewView = previewView,
                analysisExecutor = analysisExecutor,
                scanner = scanner,
                hasDetected = hasDetected,
                onDetected = onDetected,
                onError = onError,
            )
        },
    )

}

@OptIn(ExperimentalGetImage::class)
@SuppressLint("UnsafeOptInUsageError")
private fun bindCamera(
    previewView: PreviewView,
    analysisExecutor: java.util.concurrent.Executor,
    scanner: BarcodeScanner,
    hasDetected: AtomicBoolean,
    onDetected: (String) -> Unit,
    onError: (String) -> Unit,
) {
    val context = previewView.context
    val providerFuture = ProcessCameraProvider.getInstance(context)
    providerFuture.addListener(
        Runnable {
            val cameraProvider = try {
                providerFuture.get()
            } catch (cause: Throwable) {
                Log.w(LOG_TAG, "ProcessCameraProvider.getInstance failed", cause)
                onError("Camera service unavailable: ${cause.message ?: "unknown"}")
                return@Runnable
            }

            val preview = Preview.Builder().build().also { p ->
                p.setSurfaceProvider(previewView.surfaceProvider)
            }

            val analysis = ImageAnalysis.Builder()
                .setBackpressureStrategy(ImageAnalysis.STRATEGY_KEEP_ONLY_LATEST)
                .build()
            analysis.setAnalyzer(analysisExecutor) { imageProxy ->
                processFrame(
                    imageProxy = imageProxy,
                    scanner = scanner,
                    hasDetected = hasDetected,
                    onDetected = onDetected,
                )
            }

            // Resolve the host LifecycleOwner via the
            // PreviewView's context — Compose's
            // `LocalLifecycleOwner` would be the cleaner path
            // but `bindCamera` is plain Kotlin (called from
            // `AndroidView.update`'s lambda), so we walk the
            // context chain via `ContextCompat.getMainExecutor`'s
            // owning Activity instead. This is the canonical
            // CameraX-on-Compose recipe.
            val lifecycleOwner = previewView.findViewTreeLifecycleOwner()
            if (lifecycleOwner == null) {
                onError("Camera bind failed: no LifecycleOwner")
                return@Runnable
            }

            try {
                // Unbind any prior bindings so re-mounts don't
                // double-bind the analyser.
                cameraProvider.unbindAll()
                cameraProvider.bindToLifecycle(
                    lifecycleOwner,
                    CameraSelector.DEFAULT_BACK_CAMERA,
                    preview,
                    analysis,
                )
            } catch (cause: Throwable) {
                Log.w(LOG_TAG, "Camera bindToLifecycle failed", cause)
                onError("Camera bind failed: ${cause.message ?: "unknown"}")
            }
        },
        ContextCompat.getMainExecutor(context),
    )
}

/** ML Kit analyser callback. Decodes the proxied YUV frame
 *  into an [InputImage] + asks the QR-only scanner for hits;
 *  the FIRST hit fires [onDetected] and latches `hasDetected`
 *  so subsequent frames short-circuit until the analyser is
 *  unbound (host composable disposes). */
@OptIn(ExperimentalGetImage::class)
private fun processFrame(
    imageProxy: ImageProxy,
    scanner: BarcodeScanner,
    hasDetected: AtomicBoolean,
    onDetected: (String) -> Unit,
) {
    if (hasDetected.get()) {
        imageProxy.close()
        return
    }
    val mediaImage = imageProxy.image
    if (mediaImage == null) {
        imageProxy.close()
        return
    }
    val image = InputImage.fromMediaImage(
        mediaImage,
        imageProxy.imageInfo.rotationDegrees,
    )
    scanner.process(image)
        .addOnSuccessListener { barcodes ->
            if (hasDetected.get()) return@addOnSuccessListener
            for (barcode in barcodes) {
                if (barcode.format != Barcode.FORMAT_QR_CODE) continue
                val raw = barcode.rawValue ?: continue
                if (hasDetected.compareAndSet(false, true)) {
                    onDetected(raw)
                }
                break
            }
        }
        .addOnFailureListener { cause ->
            // Don't escalate per-frame failures to the UI —
            // ML Kit occasionally fails on a partial frame
            // and recovers on the next one.
            Log.v(LOG_TAG, "ML Kit barcode-scan failure", cause)
        }
        .addOnCompleteListener {
            // CameraX requires every ImageProxy be closed
            // before the next frame is delivered.
            imageProxy.close()
        }
}

private const val LOG_TAG = "QrScannerComposable"
