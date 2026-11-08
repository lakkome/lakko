// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import android.content.Context
import android.content.SharedPreferences
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow

/**
 * The camera background a person chose (`docs/ux/call-backgrounds.md`,
 * ADR-0089): what the stored text means. The reading and writing of the
 * text is the Rust core's ([BackgroundChoiceCodec]); this client only
 * carries the result.
 */
internal sealed class BackgroundSelection {
    /** The camera as it is. */
    object None : BackgroundSelection()

    /** The room blurred ([strong]: so nothing in it can be told apart). */
    data class Blur(val strong: Boolean) : BackgroundSelection()

    /** One of the six built-in pictures. */
    data class Builtin(val id: String) : BackgroundSelection()

    /** One of the person's own photos ([BackgroundPhotoStore]). */
    data class Photo(val id: String) : BackgroundSelection()

    /** Whether the room is to be replaced: every choice but [None]. */
    val isBackground: Boolean get() = this !is None
}

/**
 * What the compositor draws behind the person, decided at call time from a
 * [BackgroundSelection]: a selected photo that cannot be found turns into
 * the plain blur, never into the camera as it is (UX section 2.3, the
 * contract's "unreadable photo -> blur" rule).
 */
internal sealed class BackgroundLook {
    data class Blur(val strong: Boolean) : BackgroundLook()
    data class Builtin(val id: String) : BackgroundLook()
    data class Photo(val id: String) : BackgroundLook()
}

/** [selection] as the compositor draws it; `null` for no background.
 *  [photoReadable] answers whether an own photo is there to read. */
internal fun backgroundLookFor(
    selection: BackgroundSelection,
    photoReadable: (id: String) -> Boolean,
): BackgroundLook? = when (selection) {
    BackgroundSelection.None -> null
    is BackgroundSelection.Blur -> BackgroundLook.Blur(selection.strong)
    is BackgroundSelection.Builtin -> BackgroundLook.Builtin(selection.id)
    is BackgroundSelection.Photo ->
        if (runCatching { photoReadable(selection.id) }.getOrDefault(false)) {
            BackgroundLook.Photo(selection.id)
        } else {
            BackgroundLook.Blur(strong = false)
        }
}

/**
 * The stored text <-> [BackgroundSelection], defined once in the Rust core
 * (`lakko-segment::Choice`, `backgroundChoiceFromStored` /
 * `backgroundChoiceToStored` on UniFFI): empty or `none` is no background,
 * any other text that cannot be read is the blur.
 */
internal interface BackgroundChoiceCodec {
    fun read(stored: String): BackgroundSelection
    fun write(selection: BackgroundSelection): String
}

/** The codec over the UniFFI bindings. */
internal object NativeBackgroundChoiceCodec : BackgroundChoiceCodec {
    override fun read(stored: String): BackgroundSelection = try {
        val choice = uniffi.lakko_ffi_uniffi.backgroundChoiceFromStored(stored)
        when (choice.kind) {
            uniffi.lakko_ffi_uniffi.BackgroundKind.NONE -> BackgroundSelection.None
            uniffi.lakko_ffi_uniffi.BackgroundKind.BLUR -> BackgroundSelection.Blur(strong = false)
            uniffi.lakko_ffi_uniffi.BackgroundKind.STRONG_BLUR -> BackgroundSelection.Blur(strong = true)
            uniffi.lakko_ffi_uniffi.BackgroundKind.BUILTIN -> BackgroundSelection.Builtin(choice.id)
            uniffi.lakko_ffi_uniffi.BackgroundKind.PHOTO -> BackgroundSelection.Photo(choice.id)
        }
    } catch (_: Throwable) {
        // The rule could not be asked (no native library): hide the room
        // unless nothing was ever chosen.
        if (stored.isEmpty() || stored == "none") BackgroundSelection.None else BackgroundSelection.Blur(strong = false)
    }

    override fun write(selection: BackgroundSelection): String {
        val (kind, id) = when (selection) {
            BackgroundSelection.None -> uniffi.lakko_ffi_uniffi.BackgroundKind.NONE to ""
            is BackgroundSelection.Blur ->
                (if (selection.strong) uniffi.lakko_ffi_uniffi.BackgroundKind.STRONG_BLUR else uniffi.lakko_ffi_uniffi.BackgroundKind.BLUR) to ""
            is BackgroundSelection.Builtin -> uniffi.lakko_ffi_uniffi.BackgroundKind.BUILTIN to selection.id
            is BackgroundSelection.Photo -> uniffi.lakko_ffi_uniffi.BackgroundKind.PHOTO to selection.id
        }
        return uniffi.lakko_ffi_uniffi.backgroundChoiceToStored(
            uniffi.lakko_ffi_uniffi.BackgroundChoice(kind = kind, id = id),
        )
    }
}

/**
 * Where the choice is kept: this device only, in its own preferences file
 * (not per account: it is about this phone's camera, like the noise
 * switch), never synced and never sent (UX section 8).
 */
internal interface BackgroundChoiceStorage {
    /** The stored text; `null` when nothing was ever stored. Throws when
     *  the storage cannot be read. */
    fun read(): String?
    fun write(text: String)
}

internal class PrefsBackgroundChoiceStorage(private val prefs: SharedPreferences) : BackgroundChoiceStorage {
    override fun read(): String? = prefs.getString(KEY, null)
    override fun write(text: String) {
        prefs.edit().putString(KEY, text).apply()
    }

    companion object {
        const val PREFS = "lakko_call_video"
        const val KEY = "background"
    }
}

/**
 * The stored choice with what reads it. A storage that cannot be read
 * reads as the blur (fail toward hiding the room); a choice made while the
 * storage is broken still holds for this process.
 */
internal class BackgroundChoiceHolder(
    private val codec: BackgroundChoiceCodec,
    private val storage: () -> BackgroundChoiceStorage?,
) {
    private val state = MutableStateFlow<BackgroundSelection>(BackgroundSelection.None)
    @Volatile private var loaded = false

    /**
     * Bumped on every choice made, including the same one again: a person
     * picking a background again restarts a failed processing (UX section 3).
     */
    @Volatile var generation: Long = 0L
        private set

    val selection: StateFlow<BackgroundSelection> = state.asStateFlow()

    /** The current choice, read from storage on first use. */
    fun current(): BackgroundSelection {
        if (!loaded) load()
        return state.value
    }

    @Synchronized
    fun load() {
        if (loaded) return
        // No storage yet (the preferences are opened by the first screen
        // that needs them): nothing read, asked again later.
        val store = storage() ?: return
        val text = try {
            store.read()
        } catch (_: Throwable) {
            UNREADABLE
        }
        state.value = codec.read(text ?: "")
        loaded = true
    }

    @Synchronized
    fun select(selection: BackgroundSelection) {
        loaded = true
        state.value = selection
        generation += 1
        try {
            storage()?.write(codec.write(selection))
        } catch (_: Throwable) {
            // The choice still holds for this process.
        }
    }

    private companion object {
        /** A text no codec reads as a choice: a storage that throws on
         *  read is judged as an unreadable choice, which is the blur. */
        const val UNREADABLE = "\u0000unreadable"
    }
}

/** The process-wide stored choice. */
internal object CameraBackgroundSetting {
    @Volatile private var prefs: SharedPreferences? = null

    val holder: BackgroundChoiceHolder = BackgroundChoiceHolder(
        codec = NativeBackgroundChoiceCodec,
        storage = { prefs?.let(::PrefsBackgroundChoiceStorage) },
    )

    /** Opens the preferences file once; later calls are no-ops. */
    fun attach(context: Context) {
        if (prefs != null) return
        synchronized(this) {
            if (prefs != null) return
            prefs = try {
                context.applicationContext.getSharedPreferences(PrefsBackgroundChoiceStorage.PREFS, Context.MODE_PRIVATE)
            } catch (_: Throwable) {
                null
            }
        }
        holder.load()
    }
}
