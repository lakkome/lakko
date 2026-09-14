// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.media

import android.content.Context
import android.media.AudioAttributes
import android.media.MediaPlayer
import android.media.PlaybackParams
import android.util.Log
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.Deferred
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.async
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import me.lakko.app.chat.AttachmentBridge
import uniffi.lakko_ffi_uniffi.AttachmentRef
import uniffi.lakko_ffi_uniffi.ThumbRef
import java.io.File

/**
 * ADR-0047 — the chat screen's media runtime, shared by every media bubble:
 *
 *   * decrypted media as files in one [ChatMediaCache] the screen disposes when
 *     it leaves, so no decrypted plaintext outlives the chat that opened it;
 *   * one download per blob however many bubbles ask;
 *   * the one voice player, the one-at-a-time rule ([ExclusivePlayback]) and
 *     the auto-continue to the next unplayed voice message
 *     ([nextVoiceToPlay] decides which);
 *   * the local-only "played" set (UX spec §8 — never sent).
 *
 * A file is only ever handed to a decoder or player after the MIME allowlist
 * accepted its class ([renderDecision] at the bubble, [loadThumb] here), and
 * only after the AEAD open succeeded: [AttachmentBridge.downloadAndDecrypt] is
 * all-or-nothing, so a partial or unauthenticated body is never written.
 */
public class ChatMediaHub(
    context: Context,
    private val scope: CoroutineScope,
    public val allowlist: MediaAllowlist = coreAllowlist,
) {
    private val appContext: Context = context.applicationContext
    public val prefs: MediaPrefs = MediaPrefs.get(appContext)
    private val cache = ChatMediaCache(appContext.cacheDir).also {
        ChatMediaCache.sweepStale(appContext.cacheDir, keep = it)
    }
    public val exclusive: ExclusivePlayback = ExclusivePlayback()
    /** One storage client for every download in this chat, closed on
     *  [dispose] — not one per bubble. */
    private val storage = me.lakko.app.storage.StorageClient()
    private val inflight = HashMap<String, Deferred<File?>>()

    private val playedVersion = MutableStateFlow(0)

    /** Bumped when a played mark changes, so bubbles re-read [isPlayed]. */
    public val playedChanges: StateFlow<Int> = playedVersion.asStateFlow()

    /** Supplies the timeline's voice messages in order, for auto-continue. */
    public var voiceQueue: () -> List<VoiceQueueItem> = { emptyList() }

    // ----- media files ------------------------------------------------------

    /** Download, AEAD-open and file [ref]; `null` on any failure. */
    public suspend fun load(ref: AttachmentRef): File? =
        fetchAndFile(mediaKey(ref.blobId), ref.name, "media-${mediaKey(ref.blobId).take(8)}") {
            AttachmentBridge.downloadAndDecrypt(ref, storage)
        }

    /** Same for a video-note poster; `null` when its type is not an
     *  allowlisted image. */
    public suspend fun loadThumb(thumb: ThumbRef): File? {
        if (inlineKindOf(thumb.mime, allowlist) != InlineKind.IMAGE) return null
        return fetchAndFile("thumb-${mediaKey(thumb.blobId)}", null, "poster") {
            AttachmentBridge.downloadAndDecryptThumb(thumb, storage)
        }
    }

    /** A file already filed under [key], or `null`. */
    public fun fileFor(key: String): File? = cache.get(key)

    /** File local bytes (an outgoing take or a picked GIF) under [key]. */
    public fun fileLocal(key: String, name: String, bytes: ByteArray): File? =
        cache.put(key, name, "local", bytes)

    /** Adopt a finished recording already written under [newEntryFile]. */
    public fun adoptLocal(key: String, file: File): File? = cache.adopt(key, file)

    /** A fresh path inside this chat's cache for a recording in progress. */
    public fun newEntryFile(key: String, name: String): File? = cache.newEntryFile(key, name)

    public fun remove(key: String) {
        cache.remove(key)
    }

    private suspend fun fetchAndFile(
        key: String,
        name: String?,
        fallback: String,
        fetch: suspend () -> ByteArray,
    ): File? {
        cache.get(key)?.let { return it }
        // LAZY, registered, then started: a fetch that finished before it was
        // registered would otherwise leave a completed (possibly failed)
        // entry behind, and every later attempt would get that result.
        val deferred = synchronized(inflight) {
            inflight[key] ?: scope.async(Dispatchers.IO, start = CoroutineStart.LAZY) {
                try {
                    cache.put(key, name, fallback, fetch())
                } catch (e: CancellationException) {
                    throw e
                } catch (e: Throwable) {
                    // No blob id, key or name in the log line.
                    Log.w(TAG, "media download failed: ${e.javaClass.simpleName}")
                    null
                } finally {
                    synchronized(inflight) { inflight.remove(key) }
                }
            }.also {
                inflight[key] = it
                it.start()
            }
        }
        return deferred.await()
    }

    // ----- played -----------------------------------------------------------

    public fun isPlayed(key: String): Boolean = prefs.played.isPlayed(key)

    public fun markPlayed(key: String) {
        if (!prefs.played.isPlayed(key)) {
            prefs.markPlayed(key)
            playedVersion.update { it + 1 }
        }
    }

    // ----- the voice player -------------------------------------------------

    public data class VoicePlayback(
        val key: String? = null,
        val playing: Boolean = false,
        val loading: Boolean = false,
        val failedKey: String? = null,
        val positionMs: Long = 0,
        val durationMs: Long = 0,
        val rate: Float = 1f,
    )

    private val mutableVoice = MutableStateFlow(VoicePlayback())
    public val voice: StateFlow<VoicePlayback> = mutableVoice.asStateFlow()

    private var player: MediaPlayer? = null
    private var ticker: Job? = null
    private var loadJob: Job? = null

    /** Tap on a voice bubble's play button. */
    public fun toggleVoice(ref: AttachmentRef, knownDurationMs: Long, sentByMe: Boolean) {
        val key = mediaKey(ref.blobId)
        val current = mutableVoice.value
        if (current.key == key && player != null) {
            if (current.playing) pauseVoice() else resumeVoice()
            return
        }
        startVoice(ref, key, knownDurationMs, seekToFraction = null, sentByMe = sentByMe)
    }

    /** Drag / tap on the waveform. Starts playback of [ref] when it is not the
     *  current item. */
    public fun seekVoice(ref: AttachmentRef, fraction: Float, knownDurationMs: Long, sentByMe: Boolean) {
        val key = mediaKey(ref.blobId)
        val p = player
        if (mutableVoice.value.key == key && p != null) {
            val duration = p.duration.takeIf { it > 0 }?.toLong() ?: knownDurationMs
            val target = (duration * fraction.coerceIn(0f, 1f)).toLong()
            runCatching { p.seekTo(target.toInt()) }
            mutableVoice.update { it.copy(positionMs = target) }
            return
        }
        startVoice(ref, key, knownDurationMs, seekToFraction = fraction, sentByMe = sentByMe)
    }

    public fun cycleRate() {
        val next = nextPlaybackRate(mutableVoice.value.rate)
        mutableVoice.update { it.copy(rate = next) }
        applyRate()
    }

    private fun startVoice(
        ref: AttachmentRef,
        key: String,
        knownDurationMs: Long,
        seekToFraction: Float?,
        sentByMe: Boolean,
    ) {
        stopPlayer()
        exclusive.claim(key) { pauseVoice() }
        mutableVoice.update {
            VoicePlayback(key = key, loading = true, durationMs = knownDurationMs, rate = it.rate)
        }
        loadJob = scope.launch {
            val file = load(ref)
            if (mutableVoice.value.key != key) return@launch
            if (file == null) {
                mutableVoice.update { it.copy(loading = false, playing = false, failedKey = key) }
                exclusive.release(key)
                return@launch
            }
            val mp = MediaPlayer()
            try {
                mp.setAudioAttributes(
                    AudioAttributes.Builder()
                        .setUsage(AudioAttributes.USAGE_MEDIA)
                        .setContentType(AudioAttributes.CONTENT_TYPE_SPEECH)
                        .build(),
                )
                withContext(Dispatchers.IO) {
                    mp.setDataSource(file.absolutePath)
                    mp.prepare()
                }
            } catch (e: CancellationException) {
                mp.release()
                throw e
            } catch (e: Exception) {
                Log.w(TAG, "voice playback failed to prepare: ${e.javaClass.simpleName}")
                mp.release()
                mutableVoice.update { it.copy(loading = false, playing = false, failedKey = key) }
                exclusive.release(key)
                return@launch
            }
            if (mutableVoice.value.key != key) {
                mp.release()
                return@launch
            }
            player = mp
            mp.setOnCompletionListener { onVoiceCompleted(key) }
            mp.setOnErrorListener { _, _, _ ->
                stopPlayer()
                mutableVoice.update { VoicePlayback(failedKey = key, rate = it.rate) }
                exclusive.release(key)
                true
            }
            val duration = mp.duration.takeIf { it > 0 }?.toLong() ?: knownDurationMs
            if (seekToFraction != null) {
                runCatching { mp.seekTo((duration * seekToFraction.coerceIn(0f, 1f)).toInt()) }
            }
            mutableVoice.update { it.copy(loading = false, durationMs = duration, failedKey = null) }
            if (!sentByMe) markPlayed(key)
            resumeVoice()
        }
    }

    private fun resumeVoice() {
        val p = player ?: return
        val key = mutableVoice.value.key ?: return
        exclusive.claim(key) { pauseVoice() }
        applyRate()
        runCatching { p.start() }
        mutableVoice.update { it.copy(playing = true) }
        ticker?.cancel()
        ticker = scope.launch {
            while (isActive) {
                val pos = runCatching { player?.currentPosition?.toLong() }.getOrNull() ?: break
                mutableVoice.update { it.copy(positionMs = pos) }
                delay(PROGRESS_TICK_MS)
            }
        }
    }

    public fun pauseVoice() {
        val p = player
        ticker?.cancel()
        ticker = null
        if (p != null) runCatching { if (p.isPlaying) p.pause() }
        mutableVoice.update { it.copy(playing = false) }
    }

    private fun applyRate() {
        val p = player ?: return
        val rate = mutableVoice.value.rate
        runCatching {
            val wasPlaying = p.isPlaying
            p.playbackParams = PlaybackParams().setSpeed(rate)
            // Setting params on a paused player starts it on some versions.
            if (!wasPlaying && !mutableVoice.value.playing) p.pause()
        }
    }

    private fun onVoiceCompleted(key: String) {
        stopPlayer()
        exclusive.release(key)
        mutableVoice.update { VoicePlayback(rate = it.rate) }
        val next = nextVoiceToPlay(
            voiceQueue().map { it.copy(played = it.played || isPlayed(it.key)) },
            key,
        )
        mutableRequestedPlay.value = next
    }

    private val mutableRequestedPlay = MutableStateFlow<String?>(null)

    /** The voice message a bubble should start playing now (auto-continue). */
    public val requestedPlay: StateFlow<String?> = mutableRequestedPlay.asStateFlow()

    public fun consumeRequest(key: String) {
        if (mutableRequestedPlay.value == key) mutableRequestedPlay.value = null
    }

    private fun stopPlayer() {
        loadJob?.cancel()
        loadJob = null
        ticker?.cancel()
        ticker = null
        val p = player
        player = null
        if (p != null) {
            runCatching { p.stop() }
            runCatching { p.release() }
        }
    }

    /** The chat screen left: stop playback and delete every decrypted file. */
    public fun dispose() {
        stopPlayer()
        exclusive.stopAll()
        mutableVoice.value = VoicePlayback()
        synchronized(inflight) {
            inflight.values.forEach { it.cancel() }
            inflight.clear()
        }
        cache.dispose()
        runCatching { storage.close() }
    }

    public companion object {
        private const val TAG = "LakkoChatMediaHub"
        private const val PROGRESS_TICK_MS = 100L

        /** Lowercase hex of a blob id — the key every media file is filed under. */
        public fun mediaKey(blobId: ByteArray): String = AttachmentBridge.hex(blobId)
    }
}
