// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.media

import android.content.Context
import android.content.SharedPreferences
import java.io.File
import java.util.concurrent.ExecutorService
import java.util.concurrent.Executors

/**
 * ADR-0047 — per-device media preferences.
 *
 * All of these are conveniences a user can lose without harm, so every read
 * tolerates a missing or unreadable value and falls back to the spec default:
 *
 *   * the composer's record mode, mic or camera (UX spec §1, "remembered per
 *     device");
 *   * "Autoplay video notes", default OFF (§3 — autoplay downloads media);
 *   * the GIF privacy note's "Got it" (§5, "remembered per device");
 *   * the emoji Recent row and per-emoji skin-tone defaults (§4).
 *
 * The local-only "played" marks (§8) are kept in a file under
 * `noBackupFilesDir` instead, so a cloud backup never carries them to another
 * device; nothing in them is sent anywhere.
 */
public class MediaPrefs private constructor(context: Context) {

    private val prefs: SharedPreferences =
        context.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)
    private val playedFile: File = File(context.noBackupFilesDir, PLAYED_FILE)

    public var recordMode: CaptureKind
        get() = if (prefs.getString(KEY_MODE, null) == MODE_VIDEO) CaptureKind.VIDEO_NOTE else CaptureKind.VOICE
        set(value) {
            prefs.edit().putString(KEY_MODE, if (value == CaptureKind.VIDEO_NOTE) MODE_VIDEO else MODE_VOICE).apply()
        }

    public var autoplayVideoNotes: Boolean
        get() = prefs.getBoolean(KEY_AUTOPLAY, false)
        set(value) {
            prefs.edit().putBoolean(KEY_AUTOPLAY, value).apply()
        }

    public var gifNoteDismissed: Boolean
        get() = prefs.getBoolean(KEY_GIF_NOTE, false)
        set(value) {
            prefs.edit().putBoolean(KEY_GIF_NOTE, value).apply()
        }

    public var recentEmoji: List<String>
        get() = decodeRecent(prefs.getString(KEY_RECENT, null))
        set(value) {
            prefs.edit().putString(KEY_RECENT, encodeRecent(value)).apply()
        }

    public var tonePrefs: TonePrefs
        get() = decodeTonePrefs(prefs.getString(KEY_TONES, null))
        set(value) {
            prefs.edit().putString(KEY_TONES, encodeTonePrefs(value)).apply()
        }

    /** The process-wide played set, seeded from disk on first use. */
    public val played: PlayedMediaSet by lazy {
        PlayedMediaSet().also { set ->
            set.seed(runCatching { playedFile.takeIf { it.exists() }?.readText() }.getOrNull())
        }
    }

    /**
     * Mark [key] played. The set is updated in memory at once — callers read
     * it back immediately — and written to disk on [playedWriter].
     *
     * The write used to happen inline, and every caller is on the main thread:
     * the voice player marked a message played on the last step before
     * `MediaPlayer.start()`, so a file write sat between the person pressing
     * play and any sound. Nothing waits for this write; the worst a lost one
     * costs is an unplayed dot showing again.
     */
    public fun markPlayed(key: String) {
        if (!played.markPlayed(key)) return
        val snapshot = played.encode()
        playedWriter.execute { runCatching { playedFile.writeText(snapshot) } }
    }

    /** One thread, so two marks in quick succession are written in order. */
    private val playedWriter: ExecutorService = Executors.newSingleThreadExecutor { runnable ->
        Thread(runnable, "lakko-played-marks").apply { isDaemon = true }
    }

    public companion object {
        private const val PREFS_NAME = "lakko-media-prefs"
        private const val PLAYED_FILE = "lakko-media-played.txt"
        private const val KEY_MODE = "composer.recordMode"
        private const val KEY_AUTOPLAY = "media.autoplayVideoNotes"
        private const val KEY_GIF_NOTE = "gif.privacyNoteDismissed"
        private const val KEY_RECENT = "emoji.recent"
        private const val KEY_TONES = "emoji.tones"
        private const val MODE_VOICE = "voice"
        private const val MODE_VIDEO = "video_note"

        @Volatile private var instance: MediaPrefs? = null

        public fun get(context: Context): MediaPrefs =
            instance ?: synchronized(this) {
                instance ?: MediaPrefs(context.applicationContext).also { instance = it }
            }
    }
}

/** The bundled emoji dataset, parsed once per process from app assets. */
public object EmojiAssets {
    public const val ASSET_PATH: String = "emoji/emoji-data.json"

    @Volatile private var cached: EmojiDataset? = null

    /** Parse on first call; an unreadable asset is an empty dataset (the
     *  picker then shows `No emoji found`). Call off the main thread. */
    public fun load(context: Context): EmojiDataset =
        cached ?: synchronized(this) {
            cached ?: run {
                val text = runCatching {
                    context.assets.open(ASSET_PATH).use { it.readBytes().toString(Charsets.UTF_8) }
                }.getOrNull()
                val dataset = if (text == null) EmojiDataset.EMPTY else parseEmojiDataset(text)
                if (text != null) cached = dataset
                dataset
            }
        }
}
