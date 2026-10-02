// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.components

import android.app.Activity
import android.content.Context
import android.content.Intent
import android.net.Uri
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContract
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.platform.LocalContext
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch

/**
 * Lets a FILE leave the app: a saved attachment, an exported feed, the
 * account's data export.
 *
 * The design system keeps operating-system surfaces out of Lakko screens
 * unless the platform leaves no other way, and for a file it leaves none: an
 * app cannot write into a person's files without asking where. So this uses
 * the narrowest mechanism there is, the system's save picker
 * (`ACTION_CREATE_DOCUMENT`): the person picks a place and a name, and the
 * app is handed that one document to write. It is not a share sheet: nothing
 * is offered to other apps, and no copy is left in a cache for a provider to
 * serve.
 */
public class FileSaver internal constructor(
    private val launch: (name: String, mime: String, bytes: ByteArray) -> Unit,
) {
    /** Ask where to save [bytes], suggesting [name], and write them there. */
    public fun save(name: String, mime: String?, bytes: ByteArray) {
        launch(name, mime?.takeIf { it.isNotBlank() } ?: DEFAULT_MIME, bytes)
    }

    private companion object {
        private const val DEFAULT_MIME = "application/octet-stream"
    }
}

/** A [FileSaver] for this screen. */
@Composable
public fun rememberFileSaver(): FileSaver {
    val context = LocalContext.current.applicationContext
    // The bytes wait here while the picker is up. A plain holder, not saved
    // state: a file can be megabytes, and a picker that outlived the process
    // has nothing left to write.
    val pending = remember { PendingSave() }
    val launcher = rememberLauncherForActivityResult(CreateDocument()) { uri ->
        val bytes = pending.take()
        if (uri != null && bytes != null) writeTo(context, uri, bytes)
    }
    return remember(launcher) {
        FileSaver { name, mime, bytes ->
            pending.put(bytes)
            runCatching { launcher.launch(SaveRequest(name = name, mime = mime)) }
                .onFailure {
                    pending.take()
                    android.util.Log.w(TAG, "no save picker on this device", it)
                }
        }
    }
}

private class PendingSave {
    private var bytes: ByteArray? = null

    fun put(value: ByteArray) {
        bytes = value
    }

    fun take(): ByteArray? = bytes.also { bytes = null }
}

private data class SaveRequest(val name: String, val mime: String)

/** `ACTION_CREATE_DOCUMENT` with the type chosen per file. */
private class CreateDocument : ActivityResultContract<SaveRequest, Uri?>() {
    override fun createIntent(context: Context, input: SaveRequest): Intent =
        Intent(Intent.ACTION_CREATE_DOCUMENT)
            .addCategory(Intent.CATEGORY_OPENABLE)
            .setType(input.mime)
            .putExtra(Intent.EXTRA_TITLE, input.name)

    override fun parseResult(resultCode: Int, intent: Intent?): Uri? =
        intent?.data.takeIf { resultCode == Activity.RESULT_OK }
}

private fun writeTo(context: Context, uri: Uri, bytes: ByteArray) {
    CoroutineScope(Dispatchers.IO).launch {
        runCatching {
            context.contentResolver.openOutputStream(uri)?.use { it.write(bytes) }
        }.onFailure { android.util.Log.w(TAG, "could not write the saved file", it) }
    }
}

private const val TAG = "FileSaver"
