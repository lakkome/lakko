// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.settings

import android.app.Activity
import android.app.KeyguardManager
import android.content.Context
import android.os.Build
import android.os.CancellationSignal
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.runtime.Composable
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.ui.platform.LocalContext

/**
 * The device's own authentication of its owner: fingerprint, face, or the
 * screen lock (PIN, pattern, password), through the system prompt. The gate
 * in front of "Show recovery phrase".
 *
 * The platform prompt is used directly (no extra library): the system
 * biometric prompt with the device credential allowed from Android 10, and
 * the system "confirm your screen lock" screen before that.
 */

/** How one authentication request ended. */
public enum class DeviceAuthResult {
    /** The owner authenticated. */
    Authenticated,

    /** The prompt was dismissed or the attempt failed for good. */
    NotAuthenticated,

    /** The device has no screen lock at all, so there is nothing to ask. */
    NoDeviceLock,
}

/** Whether the device has a screen lock (PIN, pattern, password) set. */
public fun deviceHasLock(context: Context): Boolean =
    (context.getSystemService(Context.KEYGUARD_SERVICE) as? KeyguardManager)?.isDeviceSecure == true

/**
 * A function that asks the device to authenticate its owner, showing
 * [title], and reports through [onResult] (on the main thread).
 */
@Composable
public fun rememberDeviceAuthenticator(
    title: String,
    onResult: (DeviceAuthResult) -> Unit,
): () -> Unit {
    val context = LocalContext.current
    val report = rememberUpdatedState(onResult)
    val confirmCredential = rememberLauncherForActivityResult(
        ActivityResultContracts.StartActivityForResult(),
    ) { result ->
        report.value(
            if (result.resultCode == Activity.RESULT_OK) {
                DeviceAuthResult.Authenticated
            } else {
                DeviceAuthResult.NotAuthenticated
            },
        )
    }
    return {
        when {
            !deviceHasLock(context) -> report.value(DeviceAuthResult.NoDeviceLock)
            Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q -> authenticateWithSystemPrompt(context, title) {
                report.value(it)
            }
            else -> {
                val keyguard = context.getSystemService(Context.KEYGUARD_SERVICE) as? KeyguardManager

                @Suppress("DEPRECATION")
                val intent = keyguard?.createConfirmDeviceCredentialIntent(title, null)
                if (intent == null) {
                    report.value(DeviceAuthResult.NoDeviceLock)
                } else {
                    confirmCredential.launch(intent)
                }
            }
        }
    }
}

@androidx.annotation.RequiresApi(Build.VERSION_CODES.Q)
private fun authenticateWithSystemPrompt(
    context: Context,
    title: String,
    onResult: (DeviceAuthResult) -> Unit,
) {
    val builder = android.hardware.biometrics.BiometricPrompt.Builder(context).setTitle(title)
    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
        builder.setAllowedAuthenticators(
            android.hardware.biometrics.BiometricManager.Authenticators.BIOMETRIC_WEAK or
                android.hardware.biometrics.BiometricManager.Authenticators.DEVICE_CREDENTIAL,
        )
    } else {
        @Suppress("DEPRECATION")
        builder.setDeviceCredentialAllowed(true)
    }
    val callback = object : android.hardware.biometrics.BiometricPrompt.AuthenticationCallback() {
        override fun onAuthenticationSucceeded(
            result: android.hardware.biometrics.BiometricPrompt.AuthenticationResult?,
        ) {
            onResult(DeviceAuthResult.Authenticated)
        }

        override fun onAuthenticationError(errorCode: Int, errString: CharSequence?) {
            onResult(DeviceAuthResult.NotAuthenticated)
        }
        // onAuthenticationFailed: one rejected attempt; the prompt stays up.
    }
    try {
        builder.build().authenticate(CancellationSignal(), context.mainExecutor, callback)
    } catch (_: Throwable) {
        onResult(DeviceAuthResult.NotAuthenticated)
    }
}
