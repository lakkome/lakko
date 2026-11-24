// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import android.content.Context
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import kotlinx.serialization.Serializable
import kotlinx.serialization.builtins.ListSerializer
import kotlinx.serialization.json.Json
import java.io.File
import java.security.KeyStore
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

/*
 * docs/ux/connection-state.md section 7.3 — the outbox.
 *
 * Every message of this device that has not reached the server (waiting for
 * the network, or "Not sent") is an entry here, written before its first
 * attempt and on every change of its state or of its count of automatic
 * sends, deleted when it is published or when the person deletes the unsent
 * row. Entries are read back at a session landing, so a waiting message
 * survives quitting the app and goes by itself when the servers answer again.
 *
 * A media message (voice message, video note, GIF) is an entry too, before
 * its upload: the prepared file is stored as a file of its own under the
 * device key until its attachment reference exists. Text and media of one
 * conversation go in one order, oldest first ([WaitingSendPasses]).
 *
 * Stored sealed under the device's Keystore key (the entries are message
 * plaintext), one file per account plus one file per prepared file, under
 * `noBackupFilesDir`, every write through the account store gate (ADR-0077).
 * A write that fails never blocks the send: the entry stays in memory and the
 * message still goes in this process.
 */

/** The stored state of an unsent message. In flight is not a stored state:
 *  a message whose attempt was cut short by the process ending is waiting. */
@Serializable
public enum class OutboxState { WAITING, FAILED }

/** A media message's prepared file and its metadata, before (and after) its
 *  upload. */
@Serializable
public data class OutboxMedia(
    val kind: String,
    val mime: String,
    val name: String,
    val durationMs: Long? = null,
    val waveformB64: String? = null,
    val width: Int? = null,
    val height: Int? = null,
    /** The prepared bytes, as an outbox file; `null` once uploaded. */
    val fileId: String? = null,
    /** A GIF's proxy path, fetched at upload (the bytes are not on the
     *  device yet when the GIF is tapped). */
    val sourceUrl: String? = null,
    /** A video note's poster frame, as an outbox file. */
    val posterFileId: String? = null,
    val posterMime: String? = null,
    val posterWidth: Int? = null,
    val posterHeight: Int? = null,
    /** The poster was offered to storage once (a failed poster upload sends
     *  the note without one, ADR-0047). */
    val posterTried: Boolean = false,
    /** The uploaded poster. */
    val thumb: PersistedThumb? = null,
    /** The told refusal behind a "Not sent" (the sending limit, a full
     *  server), as a [me.lakko.app.media.UploadRefusal] name. */
    val refusal: String? = null,
)

/**
 * docs/ux/media-messages.md section 11 — a photo, video or file of the
 * composer tray whose upload had not finished when Send was pressed: its
 * prepared file, uploaded as part of the message's send.
 */
@Serializable
public data class OutboxFile(
    /** Its place among the message's attachments (the tray's order). */
    val index: Int,
    /** The prepared bytes, as an outbox file; `null` once uploaded. */
    val fileId: String? = null,
    val mime: String? = null,
    val name: String? = null,
    /** A photo's or video's size, `null` when unknown. */
    val width: Int? = null,
    val height: Int? = null,
    /** The sent thumbnail (a photo's or video's, an audio file's cover), as
     *  an outbox file. */
    val thumbFileId: String? = null,
    val thumbWidth: Int? = null,
    val thumbHeight: Int? = null,
    /** The thumbnail was offered to storage once (a failed thumbnail never
     *  blocks the send, section 10). */
    val thumbTried: Boolean = false,
    val thumb: PersistedThumb? = null,
    /** ADR-0047 section 6 — the micro thumbnail carried inside the message. */
    val inlineThumbB64: String? = null,
    /** An audio file (ADR-0092): its length, title and artist. */
    val audio: Boolean = false,
    val audioDurationMs: Long? = null,
    val title: String? = null,
    val artist: String? = null,
    /** The uploaded reference. */
    val uploaded: PersistedAttachment? = null,
)

/** One unsent message. */
@Serializable
public data class OutboxEntry(
    /** Stable identity of the entry (the row's local id changes when it is
     *  read back into a new session). */
    val id: String,
    val groupIdHex: String,
    /** The row's local id in this session. */
    val localId: Long,
    /** Written order within the account. */
    val seq: Long,
    val writtenAtMs: Long,
    val state: OutboxState = OutboxState.WAITING,
    /** Automatic sends that failed for a network reason (section 7.2). */
    val autoSends: Int = 0,
    val text: String = "",
    /** The attachment references (a media message has its one reference
     *  once uploaded). */
    val attachments: List<PersistedAttachment> = emptyList(),
    val reply: PersistedReply? = null,
    val forward: PersistedForward? = null,
    /** ADR-0092 — a content message, as its composed envelope. */
    val contentEnvelopeB64: String? = null,
    /** ADR-0087 section 14 — a channel invitation card. */
    val channelInviteB64: String? = null,
    val media: OutboxMedia? = null,
    /** Tray items not uploaded when Send was pressed; [attachments] holds the
     *  uploaded ones and, once every file is up, all of them in order. */
    val files: List<OutboxFile> = emptyList(),
)

private val outboxJson = Json { ignoreUnknownKeys = true }

/** At most this many entries per account; the oldest beyond it are dropped
 *  from the stored file (they still go in this process). Chosen, not
 *  measured. */
public const val MAX_STORED_OUTBOX_ENTRIES: Int = 500

internal fun encodeOutbox(entries: List<OutboxEntry>): String =
    outboxJson.encodeToString(
        ListSerializer(OutboxEntry.serializer()),
        entries.takeLast(MAX_STORED_OUTBOX_ENTRIES),
    )

/** A text that is not this schema reads as an empty outbox. */
internal fun decodeOutbox(raw: String?): List<OutboxEntry> {
    if (raw.isNullOrEmpty()) return emptyList()
    return try {
        outboxJson.decodeFromString(ListSerializer(OutboxEntry.serializer()), raw)
            .filter { HEX_GROUP.matches(it.groupIdHex) }
    } catch (_: Exception) {
        emptyList()
    }
}

private val HEX_GROUP = Regex("^[0-9a-f]+$")

// ---------------------------------------------------------------- storage

/** Seals and opens the outbox's files. */
public interface OutboxCipher {
    /** `null` when sealing failed. */
    public fun seal(plain: ByteArray): ByteArray?

    /** `null` when the bytes do not open (another key, damaged). */
    public fun open(sealed: ByteArray): ByteArray?
}

/** AES-256-GCM, `iv(12) || ciphertext || tag`, under the key [key] returns. */
public class AesGcmOutboxCipher(private val key: () -> SecretKey) : OutboxCipher {
    override fun seal(plain: ByteArray): ByteArray? = try {
        val cipher = Cipher.getInstance(AES_GCM_NOPAD)
        cipher.init(Cipher.ENCRYPT_MODE, key())
        val iv = cipher.iv
        if (iv.size != GCM_IV_LEN) null else iv + cipher.doFinal(plain)
    } catch (_: Throwable) {
        null
    }

    override fun open(sealed: ByteArray): ByteArray? = try {
        if (sealed.size <= GCM_IV_LEN) {
            null
        } else {
            val cipher = Cipher.getInstance(AES_GCM_NOPAD)
            cipher.init(Cipher.DECRYPT_MODE, key(), GCMParameterSpec(GCM_TAG_BITS, sealed.copyOfRange(0, GCM_IV_LEN)))
            cipher.doFinal(sealed.copyOfRange(GCM_IV_LEN, sealed.size))
        }
    } catch (_: Throwable) {
        null
    }

    public companion object {
        private const val AES_GCM_NOPAD = "AES/GCM/NoPadding"
        private const val GCM_IV_LEN = 12
        private const val GCM_TAG_BITS = 128

        /** Own alias, versioned, so rotating it touches nothing else. */
        public const val KEYSTORE_ALIAS: String = "me.lakko.outbox-wrap.v1"

        /** The device's Keystore key (created on first use). */
        public fun keystore(): AesGcmOutboxCipher = AesGcmOutboxCipher(::keystoreKey)

        private fun keystoreKey(): SecretKey {
            val ks = KeyStore.getInstance("AndroidKeyStore").apply { load(null) }
            (ks.getKey(KEYSTORE_ALIAS, null) as? SecretKey)?.let { return it }
            val spec = KeyGenParameterSpec.Builder(
                KEYSTORE_ALIAS,
                KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT,
            )
                .setKeySize(256)
                .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
                .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
                .setRandomizedEncryptionRequired(true)
                .setUserAuthenticationRequired(false)
                .build()
            val kg = KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, "AndroidKeyStore")
            kg.init(spec)
            return kg.generateKey()
        }
    }
}

/** Where the outbox lives between sessions. */
public interface OutboxBackend {
    public fun load(accountIdB64: String): List<OutboxEntry>

    /** `false` when the entries could not be written. */
    public fun save(accountIdB64: String, entries: List<OutboxEntry>): Boolean

    public fun writeFile(accountIdB64: String, fileId: String, bytes: ByteArray): Boolean

    public fun readFile(accountIdB64: String, fileId: String): ByteArray?

    public fun deleteFile(accountIdB64: String, fileId: String)

    /** Forget the account's outbox, its files included. */
    public fun deleteAccount(accountIdB64: String)
}

/**
 * The production store: `<root>/<account>/entries.bin` and
 * `<root>/<account>/files/<id>.bin`, each sealed by [cipher]. Writes go
 * through [me.lakko.app.account.AccountStoreGate], so an account whose session
 * ended takes no late write.
 */
public class FileOutboxStore(
    /** Read on first use (the repository is built before a context with
     *  storage is needed). */
    rootDir: () -> File,
    private val cipher: OutboxCipher,
) : OutboxBackend {
    public constructor(root: File, cipher: OutboxCipher) : this({ root }, cipher)

    private val root: File by lazy(rootDir)
    private val lock = Any()

    override fun load(accountIdB64: String): List<OutboxEntry> = synchronized(lock) {
        val f = entriesFile(accountIdB64)
        if (!f.exists()) return emptyList()
        val plain = try {
            cipher.open(f.readBytes())
        } catch (_: Throwable) {
            null
        } ?: return emptyList()
        decodeOutbox(String(plain, Charsets.UTF_8))
    }

    override fun save(accountIdB64: String, entries: List<OutboxEntry>): Boolean =
        me.lakko.app.account.AccountStoreGate.write(accountIdB64) {
            synchronized(lock) {
                val target = entriesFile(accountIdB64)
                if (entries.isEmpty()) {
                    target.delete()
                    true
                } else {
                    val sealed = cipher.seal(encodeOutbox(entries).toByteArray(Charsets.UTF_8))
                    sealed != null && writeAtomically(target, sealed)
                }
            }
        } ?: false

    override fun writeFile(accountIdB64: String, fileId: String, bytes: ByteArray): Boolean =
        me.lakko.app.account.AccountStoreGate.write(accountIdB64) {
            val sealed = cipher.seal(bytes)
            sealed != null && synchronized(lock) { writeAtomically(dataFile(accountIdB64, fileId), sealed) }
        } ?: false

    override fun readFile(accountIdB64: String, fileId: String): ByteArray? {
        val f = synchronized(lock) { dataFile(accountIdB64, fileId) }
        if (!f.exists()) return null
        return try {
            cipher.open(f.readBytes())
        } catch (_: Throwable) {
            null
        }
    }

    override fun deleteFile(accountIdB64: String, fileId: String) {
        synchronized(lock) { dataFile(accountIdB64, fileId).delete() }
    }

    override fun deleteAccount(accountIdB64: String) {
        synchronized(lock) { accountDir(accountIdB64).deleteRecursively() }
    }

    private fun writeAtomically(target: File, bytes: ByteArray): Boolean = try {
        target.parentFile?.mkdirs()
        val tmp = File(target.parentFile, target.name + ".tmp")
        tmp.writeBytes(bytes)
        if (!tmp.renameTo(target)) {
            target.writeBytes(bytes)
            tmp.delete()
        }
        true
    } catch (_: Throwable) {
        false
    }

    private fun accountDir(accountIdB64: String): File =
        File(root, me.lakko.app.account.AccountStoreGate.key(accountIdB64).replace('/', '_').replace('+', '-'))

    private fun entriesFile(accountIdB64: String): File = File(accountDir(accountIdB64), "entries.bin")

    private fun dataFile(accountIdB64: String, fileId: String): File {
        require(FILE_ID.matches(fileId)) { "bad outbox file id" }
        return File(File(accountDir(accountIdB64), "files"), "$fileId.bin")
    }

    public companion object {
        private val FILE_ID = Regex("^[0-9a-f-]{1,64}$")

        /** Directory under `noBackupFilesDir`. */
        public const val DIR: String = "lakko-outbox"

        /** The device's store. */
        public fun onDevice(context: Context): FileOutboxStore =
            FileOutboxStore({ File(context.applicationContext.noBackupFilesDir, DIR) }, AesGcmOutboxCipher.keystore())
    }
}

// ----------------------------------------------------------------- engine

/** What one delivery of an entry did (the repository's side of a send). */
public sealed class DeliverOutcome {
    /** Published: the row is marked sent by the deliverer. */
    public object Sent : DeliverOutcome()

    /** Refused for a reason other than the network: "Not sent". */
    public data class Refused(val cause: Throwable?) : DeliverOutcome()

    /** The conversation is no longer held here: the entry is dropped. */
    public object Dropped : DeliverOutcome()

    /** Taken over by another queue (ADR-0073 held send): the entry is
     *  dropped, the row stays with its new owner. */
    public object Handed : DeliverOutcome()
}

/** The rows the outbox drives: the conversation's own rows in the repository. */
public interface OutboxRows {
    /** Whether the conversation's group is held on this device. */
    public fun isHeld(groupIdHex: String): Boolean

    /** Show an entry read back at a session landing as a row at the end of
     *  its conversation, in [ack]. Returns the row's local id, `null` when it
     *  cannot be shown (the entry is dropped). */
    public fun show(entry: OutboxEntry, ack: Ack, posterBytes: ByteArray?): Long?

    /** The row's state changed; [refusal] is set for a told refusal. */
    public fun setAck(groupIdHex: String, localId: Long, ack: Ack, refusal: me.lakko.app.media.UploadRefusal? = null)

    /** A row's files are uploaded: it carries every reference now, in order. */
    public fun setUploaded(groupIdHex: String, localId: Long, refs: List<uniffi.lakko_ffi_uniffi.AttachmentRef>)

    /** The row is gone (the person deleted it, or its conversation). */
    public fun remove(groupIdHex: String, localId: Long)
}

/** The storage side of a media message. */
public interface OutboxUploader {
    /** A GIF's bytes and type, through the proxy. */
    public suspend fun fetchSource(url: String): Pair<ByteArray, String>

    public suspend fun uploadThumb(bytes: ByteArray, mime: String, width: Int, height: Int): uniffi.lakko_ffi_uniffi.ThumbRef

    public suspend fun uploadMain(
        bytes: ByteArray,
        mime: String,
        name: String,
        meta: AttachmentMediaMeta?,
        onProgress: (Float) -> Unit,
    ): uniffi.lakko_ffi_uniffi.AttachmentRef
}

/** A file was missing or did not open: the message cannot go as it is. */
public class OutboxFileMissing : RuntimeException("outbox file missing")

/**
 * Section 7.3 — the outbox and the passes that send it, for every
 * conversation of the session. Owned by the repository (never a screen), so a
 * message, media included, goes on when its conversation is closed and after
 * a restart.
 */
public class OutboxQueue(
    private val scope: CoroutineScope,
    private val backend: OutboxBackend,
    private val rows: OutboxRows,
    /** Publish the entry once (its attachments uploaded). Throws for a
     *  failure the outbox classifies (no answer from the network: waits). */
    private val deliver: suspend (entry: OutboxEntry, automatic: Boolean) -> DeliverOutcome,
    private val uploader: OutboxUploader,
    private val isOnline: () -> Boolean,
    private val awaitOnline: suspend () -> Unit,
    private val onLog: (String) -> Unit = {},
    /** Blobs that were uploaded for a message never sent (ADR-0065 section 6). */
    private val deleteBlobs: (List<ByteArray>) -> Unit = {},
    recheckDelayMs: Long = WAITING_WHILE_ONLINE_RECHECK_MS,
    private val nowMs: () -> Long = System::currentTimeMillis,
    private val newId: () -> String = { java.util.UUID.randomUUID().toString() },
    /** ADR-0047 section 6 — the message's references once every file is up:
     *  the micro thumbnail stays on its carrier only. */
    private val finishAttachments: (List<uniffi.lakko_ffi_uniffi.AttachmentRef>) -> List<uniffi.lakko_ffi_uniffi.AttachmentRef> = { it },
) {
    private val lock = Any()
    private val saveLock = Any()
    private var account: String? = null
    private val entries = LinkedHashMap<Pair<String, Long>, OutboxEntry>()
    private val inFlight = HashSet<Pair<String, Long>>()

    /** Prepared files whose write failed: kept here so the send still goes
     *  in this process. */
    private val memFiles = HashMap<String, ByteArray>()
    private var nextSeq = 1L

    private val mutableProgress = MutableStateFlow<Map<String, Float>>(emptyMap())

    /** Upload progress of media rows in flight, keyed by [progressKey]. */
    public val progress: StateFlow<Map<String, Float>> = mutableProgress.asStateFlow()

    private val passes = WaitingSendPasses(
        scope = scope,
        waitingRows = { group -> waitingOf(group) },
        resend = { group, localId, automatic -> attempt(group, localId, automatic).outcome },
        markFailed = { group, localId ->
            update(group, localId) { it.copy(state = OutboxState.FAILED) }
            rows.setAck(group, localId, Ack.FAILED)
        },
        isOnline = isOnline,
        awaitOnline = awaitOnline,
        onLog = onLog,
        recheckDelayMs = recheckDelayMs,
        onNetworkFailureCount = { group, localId, count ->
            update(group, localId) { it.copy(autoSends = count) }
        },
    )

    /** The account the outbox belongs to now, `null` between sessions. */
    public fun accountNow(): String? = synchronized(lock) { account }

    /** Every entry, in written order (tests, diagnostics). */
    public fun entriesNow(): List<OutboxEntry> = synchronized(lock) { entries.values.sortedBy { it.seq } }

    /** Whether [localId] of [groupIdHex] is an outbox entry. */
    public fun has(groupIdHex: String, localId: Long): Boolean =
        synchronized(lock) { (groupIdHex to localId) in entries }

    /** How many automatic sends of the entry failed for a network reason. */
    public fun autoSendsOf(groupIdHex: String, localId: Long): Int =
        synchronized(lock) { entries[groupIdHex to localId]?.autoSends ?: 0 }

    /**
     * Read the account's outbox back (a session landing): each entry shows as
     * a row at the end of its conversation, in written order and in its state;
     * an entry of a conversation no longer held is dropped. The waiting ones go
     * once the state reads Online.
     */
    public fun restore(accountIdB64: String) {
        // Read back once per session: a second landing of the same session
        // would show every row twice.
        if (synchronized(lock) { account == accountIdB64 }) return
        val stored = try {
            backend.load(accountIdB64)
        } catch (_: Throwable) {
            emptyList()
        }.sortedBy { it.seq }
        synchronized(lock) {
            account = accountIdB64
            entries.clear()
            inFlight.clear()
            nextSeq = (stored.maxOfOrNull { it.seq } ?: 0L) + 1L
        }
        val dropped = ArrayList<OutboxEntry>()
        for (entry in stored) {
            if (!rows.isHeld(entry.groupIdHex)) {
                dropped += entry
                continue
            }
            val ack = if (entry.state == OutboxState.FAILED) Ack.FAILED else Ack.WAITING
            val poster = entry.media?.posterFileId?.let { readFile(accountIdB64, it) }
                ?: entry.files.firstOrNull()?.thumbFileId?.let { readFile(accountIdB64, it) }
            val localId = try {
                rows.show(entry, ack, poster)
            } catch (_: Throwable) {
                null
            }
            if (localId == null) {
                dropped += entry
                continue
            }
            val restored = entry.copy(localId = localId)
            synchronized(lock) { entries[entry.groupIdHex to localId] = restored }
            if (restored.state == OutboxState.WAITING) {
                passes.seedNetworkFailures(entry.groupIdHex, localId, restored.autoSends)
            }
        }
        for (entry in dropped) deleteFilesOf(accountIdB64, entry)
        save()
        val waitingGroups = synchronized(lock) {
            entries.values.filter { it.state == OutboxState.WAITING }.map { it.groupIdHex }.toSet()
        }
        if (dropped.isNotEmpty()) onLog("outbox dropped ${dropped.size} entries of conversations no longer held")
        for (group in waitingGroups) passes.waitForOnline(group, recheck = false)
    }

    /**
     * Write a new message's entry, before its first attempt. Its row is
     * already in the conversation, reading "Sending...". The entry is in
     * flight from here: nothing else sends it until [queueBehindIfWaiting]
     * or [sendNow] decides.
     */
    public fun add(
        entry: OutboxEntry,
        file: ByteArray? = null,
        poster: ByteArray? = null,
        /** The prepared bytes of [OutboxEntry.files], by file id. */
        files: Map<String, ByteArray> = emptyMap(),
    ) {
        val acct = synchronized(lock) { account }
        val all = HashMap<String, ByteArray>(files)
        entry.media?.fileId?.let { id -> if (file != null) all[id] = file }
        entry.media?.posterFileId?.let { id -> if (poster != null) all[id] = poster }
        for ((id, bytes) in all) {
            if (acct != null) writeFileOrKeep(acct, id, bytes) else synchronized(lock) { memFiles[id] = bytes }
        }
        synchronized(lock) {
            val withSeq = entry.copy(seq = nextSeq++, writtenAtMs = nowMs(), state = OutboxState.WAITING)
            entries[entry.groupIdHex to entry.localId] = withSeq
            inFlight += entry.groupIdHex to entry.localId
        }
        save()
    }

    /** A new entry for [groupIdHex]: its identity and written time. */
    public fun newEntry(groupIdHex: String, localId: Long): OutboxEntry =
        OutboxEntry(id = newId(), groupIdHex = groupIdHex, localId = localId, seq = 0L, writtenAtMs = nowMs())

    /**
     * Section 7.2 — the entry just added goes behind the conversation's
     * waiting messages when there are any (or a pass runs): it is shown
     * waiting and goes in turn. Returns whether it was queued; `false` means
     * the caller sends it now ([sendNow] / [launch]).
     */
    public fun queueBehindIfWaiting(groupIdHex: String, localId: Long): Boolean =
        passes.queueBehindIfWaiting(groupIdHex) {
            synchronized(lock) { inFlight -= groupIdHex to localId }
            update(groupIdHex, localId) { it.copy(state = OutboxState.WAITING) }
            rows.setAck(groupIdHex, localId, Ack.WAITING)
        }

    /**
     * One attempt now, by the person (a new message, or a Retry of a
     * "Not sent" one). Returns once sent or left waiting; throws the refusal
     * of a message that went "Not sent", so the caller can say why.
     */
    public suspend fun sendNow(groupIdHex: String, localId: Long) {
        val result = attempt(groupIdHex, localId, automatic = false, claimed = true)
        when (result.outcome) {
            ResendOutcome.NETWORK -> passes.waitForOnline(groupIdHex)
            ResendOutcome.REFUSED -> result.cause?.let { throw it }
            ResendOutcome.SENT, ResendOutcome.GONE -> Unit
        }
    }

    /** [sendNow] without waiting for it (a media message: its row says what
     *  happened). */
    public fun launch(groupIdHex: String, localId: Long) {
        scope.launch {
            try {
                sendNow(groupIdHex, localId)
            } catch (cancel: CancellationException) {
                throw cancel
            } catch (cause: Throwable) {
                onLog("outbox send refused: ${cause.javaClass.simpleName}")
            }
        }
    }

    /**
     * The person tapped an unsent row. A waiting one starts its
     * conversation's pass now (earlier waiting messages first); a "Not sent"
     * one goes again, behind the waiting ones. Returns `false` when the row
     * is not an outbox entry; throws a refusal like [sendNow].
     */
    public suspend fun retry(groupIdHex: String, localId: Long): Boolean {
        val key = groupIdHex to localId
        val state = synchronized(lock) {
            val entry = entries[key] ?: return false
            if (key in inFlight) return true
            if (entry.state == OutboxState.FAILED) inFlight += key
            entry.state
        }
        if (state == OutboxState.WAITING) {
            passes.request(groupIdHex, tappedLocalId = localId)
            return true
        }
        // A fresh budget of automatic sends: the person sent it again.
        passes.forgetNetworkFailures(groupIdHex, localId)
        update(groupIdHex, localId) {
            it.copy(autoSends = 0, media = it.media?.copy(refusal = null))
        }
        if (queueBehindIfWaiting(groupIdHex, localId)) return true
        sendNow(groupIdHex, localId)
        return true
    }

    /**
     * The person deleted an unsent row: its entry, its prepared files and any
     * blob already uploaded for it go. A row in flight is not deleted.
     * Returns whether the row was an outbox entry that is now gone.
     */
    public fun delete(groupIdHex: String, localId: Long): Boolean {
        val key = groupIdHex to localId
        val (entry, acct) = synchronized(lock) {
            if (key in inFlight) return false
            val e = entries.remove(key) ?: return false
            e to account
        }
        passes.forgetNetworkFailures(groupIdHex, localId)
        save()
        if (acct != null) deleteFilesOf(acct, entry)
        removeProgress(groupIdHex, localId)
        rows.remove(groupIdHex, localId)
        val blobs = (entry.attachments + entry.files.mapNotNull { it.uploaded }).flatMap { a ->
            listOfNotNull(
                runCatching { java.util.Base64.getDecoder().decode(a.blobIdB64) }.getOrNull(),
                a.thumb?.let { t -> runCatching { java.util.Base64.getDecoder().decode(t.blobIdB64) }.getOrNull() },
            )
        } + (listOfNotNull(entry.media?.thumb) + entry.files.filter { it.uploaded == null }.mapNotNull { it.thumb }).mapNotNull { t ->
            runCatching { java.util.Base64.getDecoder().decode(t.blobIdB64) }.getOrNull()
        }
        val distinct = blobs.distinctBy { it.contentHashCode() }
        if (distinct.isNotEmpty()) deleteBlobs(distinct)
        return true
    }

    /** Section 7.2 — the state entered Online: every conversation with
     *  waiting messages runs its pass. */
    public fun requestAllWaiting() {
        val groups = synchronized(lock) {
            entries.values.filter { it.state == OutboxState.WAITING }.map { it.groupIdHex }.toSet()
        }
        for (group in groups) passes.request(group)
    }

    /** The session ended: nothing of it stays in memory (the stored outbox
     *  is wiped by the account wipe, or read back at the next landing). */
    public fun clear() {
        synchronized(lock) {
            account = null
            entries.clear()
            inFlight.clear()
            memFiles.clear()
        }
        passes.clear()
        mutableProgress.value = emptyMap()
    }

    // ------------------------------------------------------------- internals

    /** What an attempt did, with the refusal behind a "Not sent". */
    private class Attempt(val outcome: ResendOutcome, val cause: Throwable? = null)

    private fun waitingOf(groupIdHex: String): List<Long> = synchronized(lock) {
        entries.values.asSequence()
            .filter { it.groupIdHex == groupIdHex && it.state == OutboxState.WAITING }
            .filter { (it.groupIdHex to it.localId) !in inFlight }
            .sortedBy { it.seq }
            .map { it.localId }
            .toList()
    }

    private suspend fun attempt(
        groupIdHex: String,
        localId: Long,
        automatic: Boolean,
        claimed: Boolean = false,
    ): Attempt {
        val key = groupIdHex to localId
        val start = synchronized(lock) {
            val e = entries[key] ?: return Attempt(ResendOutcome.GONE)
            if (!claimed) {
                if (key in inFlight) return Attempt(ResendOutcome.GONE)
                inFlight += key
            }
            e
        }
        rows.setAck(groupIdHex, localId, Ack.PENDING)
        try {
            var entry = start
            if (entry.media != null && entry.attachments.isEmpty()) entry = upload(entry)
            if (entry.files.isNotEmpty()) entry = uploadFiles(entry)
            return when (val delivered = deliver(entry, automatic)) {
                DeliverOutcome.Sent -> {
                    finish(key, entry)
                    Attempt(ResendOutcome.SENT)
                }
                DeliverOutcome.Dropped, DeliverOutcome.Handed -> {
                    finish(key, entry)
                    Attempt(ResendOutcome.GONE)
                }
                is DeliverOutcome.Refused -> {
                    update(groupIdHex, localId) { it.copy(state = OutboxState.FAILED) }
                    rows.setAck(groupIdHex, localId, Ack.FAILED)
                    Attempt(ResendOutcome.REFUSED, delivered.cause)
                }
            }
        } catch (cancel: CancellationException) {
            // Left waiting for the next pass, never "Sending..." forever.
            if (has(groupIdHex, localId)) {
                update(groupIdHex, localId) { it.copy(state = OutboxState.WAITING) }
                rows.setAck(groupIdHex, localId, Ack.WAITING)
            }
            throw cancel
        } catch (cause: Throwable) {
            if (!has(groupIdHex, localId)) return Attempt(ResendOutcome.GONE)
            val refusal = me.lakko.app.media.uploadRefusalOf(cause)
            return if (refusal == null && classifySendFailure(cause) == SendFailureKind.WAITING) {
                update(groupIdHex, localId) { it.copy(state = OutboxState.WAITING) }
                rows.setAck(groupIdHex, localId, Ack.WAITING)
                onLog("send left waiting for the network")
                Attempt(ResendOutcome.NETWORK, cause)
            } else {
                update(groupIdHex, localId) {
                    it.copy(state = OutboxState.FAILED, media = it.media?.copy(refusal = refusal?.name))
                }
                rows.setAck(groupIdHex, localId, Ack.FAILED, refusal)
                onLog("send not sent: ${cause.javaClass.simpleName}")
                Attempt(ResendOutcome.REFUSED, cause)
            }
        } finally {
            synchronized(lock) { inFlight -= key }
            removeProgress(groupIdHex, localId)
        }
    }

    /** Upload what is not uploaded yet; the entry carries its reference
     *  afterwards and its prepared files are deleted. */
    private suspend fun upload(entry: OutboxEntry): OutboxEntry {
        val media = entry.media ?: return entry
        val acct = synchronized(lock) { account }
        setProgress(entry.groupIdHex, entry.localId, 0.02f)
        var mime = media.mime
        var name = media.name
        val bytes: ByteArray = media.fileId?.let { readFile(acct, it) }
            ?: media.sourceUrl?.let { url ->
                val (b, m) = uploader.fetchSource(url)
                mime = m
                if (media.kind == me.lakko.app.media.KIND_GIF) {
                    name = me.lakko.app.media.MediaSendQueue.gifNameFor(name, m)
                }
                b
            }
            ?: throw OutboxFileMissing()
        var thumb = media.thumb?.let(::thumbRefOf)
        var posterTried = media.posterTried
        val posterId = media.posterFileId
        if (thumb == null && !posterTried && posterId != null) {
            val posterBytes = readFile(acct, posterId)
            if (posterBytes != null) {
                try {
                    thumb = uploader.uploadThumb(
                        posterBytes,
                        media.posterMime ?: "image/jpeg",
                        media.posterWidth ?: 0,
                        media.posterHeight ?: 0,
                    )
                } catch (cancel: CancellationException) {
                    throw cancel
                } catch (cause: Throwable) {
                    // No answer from the network: the whole attempt waits.
                    if (classifySendFailure(cause) == SendFailureKind.WAITING) throw cause
                    // ADR-0047 — any other failed poster upload sends the
                    // note without one; the bubble shows a neutral circle.
                    onLog("video note poster upload failed; sending without a poster")
                }
            }
            posterTried = true
            val uploadedThumb = thumb
            update(entry.groupIdHex, entry.localId) {
                it.copy(media = it.media?.copy(posterTried = true, thumb = uploadedThumb?.let(::persistedThumbOf)))
            }
        }
        val meta = AttachmentMediaMeta(
            kind = media.kind,
            durationMs = media.durationMs?.takeIf { it > 0 }?.coerceAtMost(UInt.MAX_VALUE.toLong())?.toUInt(),
            waveform = media.waveformB64?.let { runCatching { java.util.Base64.getDecoder().decode(it) }.getOrNull() },
            width = media.width?.takeIf { it in 1..me.lakko.app.media.MAX_GIF_DIMENSION }?.toUInt(),
            height = media.height?.takeIf { it in 1..me.lakko.app.media.MAX_GIF_DIMENSION }?.toUInt(),
            thumb = thumb,
        )
        val ref = uploader.uploadMain(bytes, mime, name, meta) { p ->
            setProgress(entry.groupIdHex, entry.localId, 0.05f + 0.85f * p.coerceIn(0f, 1f))
        }
        setProgress(entry.groupIdHex, entry.localId, 0.95f)
        val finalThumb = thumb
        val uploaded = update(entry.groupIdHex, entry.localId) {
            it.copy(
                attachments = listOf(PersistedAttachment.from(ref)),
                media = it.media?.copy(
                    mime = mime,
                    name = name,
                    fileId = null,
                    sourceUrl = null,
                    posterFileId = null,
                    posterTried = true,
                    thumb = finalThumb?.let(::persistedThumbOf),
                ),
            )
        } ?: entry.copy(attachments = listOf(PersistedAttachment.from(ref)))
        // The reference exists: the prepared files are no longer needed.
        if (acct != null) {
            media.fileId?.let { runCatching { backend.deleteFile(acct, it) } }
            media.posterFileId?.let { runCatching { backend.deleteFile(acct, it) } }
        }
        synchronized(lock) {
            media.fileId?.let(memFiles::remove)
            media.posterFileId?.let(memFiles::remove)
        }
        rows.setUploaded(entry.groupIdHex, entry.localId, listOf(ref))
        return uploaded
    }

    /**
     * Upload the tray files the message still carries, in order; each one's
     * reference is stored as it lands (a retry does not upload it again) and
     * its prepared files are deleted. Once every file is up, the entry and
     * the row carry all references in the tray's order.
     */
    private suspend fun uploadFiles(start: OutboxEntry): OutboxEntry {
        val acct = synchronized(lock) { account }
        val group = start.groupIdHex
        val localId = start.localId
        val total = start.files.size
        var entry = start
        for ((n, file) in start.files.withIndex()) {
            if (file.uploaded != null) continue
            val base = n.toFloat() / total
            setProgress(group, localId, 0.02f + 0.9f * base)
            val bytes = file.fileId?.let { readFile(acct, it) } ?: throw OutboxFileMissing()
            var thumb = file.thumb?.let(::thumbRefOf)
            val thumbId = file.thumbFileId
            if (thumb == null && !file.thumbTried && thumbId != null) {
                val thumbBytes = readFile(acct, thumbId)
                if (thumbBytes != null) {
                    try {
                        thumb = uploader.uploadThumb(thumbBytes, "image/jpeg", file.thumbWidth ?: 0, file.thumbHeight ?: 0)
                    } catch (cancel: CancellationException) {
                        throw cancel
                    } catch (cause: Throwable) {
                        if (classifySendFailure(cause) == SendFailureKind.WAITING) throw cause
                        // Section 10 — never block a send on a preview.
                        onLog("thumbnail upload failed; sending without one")
                    }
                }
                val t = thumb
                entry = updateFile(group, localId, file.index) {
                    it.copy(thumbTried = true, thumb = t?.let(::persistedThumbOf))
                } ?: return entry
            }
            val meta = when {
                file.audio -> AttachmentMediaMeta(
                    kind = me.lakko.app.media.KIND_AUDIO,
                    durationMs = file.audioDurationMs?.takeIf { it > 0 }?.coerceAtMost(UInt.MAX_VALUE.toLong())?.toUInt(),
                    thumb = thumb,
                )
                file.width != null || file.height != null || thumb != null -> AttachmentMediaMeta(
                    width = file.width?.toUInt(),
                    height = file.height?.toUInt(),
                    thumb = thumb,
                )
                else -> null
            }
            var ref = uploader.uploadMain(bytes, file.mime ?: "application/octet-stream", file.name ?: "file", meta) { p ->
                setProgress(group, localId, 0.02f + 0.9f * (base + p.coerceIn(0f, 1f) / total))
            }
            if (file.audio) ref = ref.copy(title = file.title ?: file.name?.substringBeforeLast('.'), artist = file.artist)
            file.inlineThumbB64
                ?.let { runCatching { java.util.Base64.getDecoder().decode(it) }.getOrNull() }
                ?.let { ref = ref.copy(inlineThumb = it) }
            val landed = ref
            entry = updateFile(group, localId, file.index) {
                it.copy(uploaded = PersistedAttachment.from(landed), fileId = null, thumbFileId = null)
            } ?: return entry
            if (acct != null) {
                file.fileId?.let { runCatching { backend.deleteFile(acct, it) } }
                file.thumbFileId?.let { runCatching { backend.deleteFile(acct, it) } }
            }
            synchronized(lock) {
                file.fileId?.let(memFiles::remove)
                file.thumbFileId?.let(memFiles::remove)
            }
        }
        // Every file is up: one list of references in the tray's order.
        val ready = entry.attachments.mapNotNull { it.toAttachmentRef() }.iterator()
        val byIndex = entry.files.associate { it.index to it.uploaded?.toAttachmentRef() }
        val size = entry.attachments.size + entry.files.size
        val merged = ArrayList<uniffi.lakko_ffi_uniffi.AttachmentRef>(size)
        for (i in 0 until size) {
            val r = if (i in byIndex) byIndex[i] else if (ready.hasNext()) ready.next() else null
            merged += r ?: throw OutboxFileMissing()
        }
        val refs = finishAttachments(merged)
        val done = update(group, localId) {
            it.copy(attachments = refs.map(PersistedAttachment.Companion::from), files = emptyList())
        } ?: return entry
        rows.setUploaded(group, localId, refs)
        setProgress(group, localId, 0.95f)
        return done
    }

    private fun updateFile(groupIdHex: String, localId: Long, index: Int, change: (OutboxFile) -> OutboxFile): OutboxEntry? =
        update(groupIdHex, localId) { e -> e.copy(files = e.files.map { if (it.index == index) change(it) else it }) }

    private fun finish(key: Pair<String, Long>, entry: OutboxEntry) {
        val acct = synchronized(lock) {
            entries.remove(key)
            account
        }
        passes.forgetNetworkFailures(key.first, key.second)
        save()
        if (acct != null) deleteFilesOf(acct, entry)
    }

    /** Change one entry and store the outbox. Returns the changed entry. */
    private fun update(groupIdHex: String, localId: Long, change: (OutboxEntry) -> OutboxEntry): OutboxEntry? {
        val changed = synchronized(lock) {
            val e = entries[groupIdHex to localId] ?: return null
            val c = change(e)
            entries[groupIdHex to localId] = c
            c
        }
        save()
        return changed
    }

    private fun save() {
        synchronized(saveLock) {
            val (acct, snapshot) = synchronized(lock) { account to entries.values.sortedBy { it.seq } }
            if (acct == null) return
            val ok = try {
                backend.save(acct, snapshot)
            } catch (_: Throwable) {
                false
            }
            if (!ok) onLog("outbox not stored")
        }
    }

    private fun writeFileOrKeep(acct: String, fileId: String, bytes: ByteArray) {
        val ok = try {
            backend.writeFile(acct, fileId, bytes)
        } catch (_: Throwable) {
            false
        }
        if (!ok) {
            onLog("outbox file not stored; kept in memory")
            synchronized(lock) { memFiles[fileId] = bytes }
        }
    }

    private fun readFile(acct: String?, fileId: String): ByteArray? {
        synchronized(lock) { memFiles[fileId] }?.let { return it }
        if (acct == null) return null
        return try {
            backend.readFile(acct, fileId)
        } catch (_: Throwable) {
            null
        }
    }

    private fun deleteFilesOf(acct: String, entry: OutboxEntry) {
        val ids = listOfNotNull(entry.media?.fileId, entry.media?.posterFileId) +
            entry.files.flatMap { listOfNotNull(it.fileId, it.thumbFileId) }
        for (id in ids) {
            try {
                backend.deleteFile(acct, id)
            } catch (_: Throwable) {
                // best-effort: the account wipe removes the directory
            }
            synchronized(lock) { memFiles.remove(id) }
        }
    }

    private fun setProgress(groupIdHex: String, localId: Long, value: Float) {
        mutableProgress.update { it + (progressKey(groupIdHex, localId) to value) }
    }

    private fun removeProgress(groupIdHex: String, localId: Long) {
        mutableProgress.update { it - progressKey(groupIdHex, localId) }
    }

    public companion object {
        /** The key of a row in [progress]. */
        public fun progressKey(groupIdHex: String, localId: Long): String = "$groupIdHex:$localId"
    }
}

/** A stored poster reference as the core's [uniffi.lakko_ffi_uniffi.ThumbRef]. */
internal fun thumbRefOf(t: PersistedThumb): uniffi.lakko_ffi_uniffi.ThumbRef? = try {
    uniffi.lakko_ffi_uniffi.ThumbRef(
        blobId = java.util.Base64.getDecoder().decode(t.blobIdB64),
        key = java.util.Base64.getDecoder().decode(t.keyB64),
        nonce = java.util.Base64.getDecoder().decode(t.nonceB64),
        mime = t.mime,
        size = t.size,
        width = t.width,
        height = t.height,
    )
} catch (_: IllegalArgumentException) {
    null
}

/** The inverse of [thumbRefOf]. */
internal fun persistedThumbOf(t: uniffi.lakko_ffi_uniffi.ThumbRef): PersistedThumb = PersistedThumb(
    blobIdB64 = java.util.Base64.getEncoder().encodeToString(t.blobId),
    keyB64 = java.util.Base64.getEncoder().encodeToString(t.key),
    nonceB64 = java.util.Base64.getEncoder().encodeToString(t.nonce),
    mime = t.mime,
    size = t.size,
    width = t.width,
    height = t.height,
)

/**
 * A voice message, video note or GIF handed to the repository to send: the
 * prepared bytes (or, for a GIF, its proxy path), its metadata and a video
 * note's poster frame.
 */
public class PreparedMedia(
    public val kind: String,
    public val mime: String,
    public val name: String,
    public val durationMs: Long? = null,
    public val waveform: ByteArray? = null,
    public val width: Int? = null,
    public val height: Int? = null,
    /** The file's bytes after any resize or re-encode; `null` for a GIF. */
    public val bytes: ByteArray? = null,
    /** A GIF's proxy path, fetched at upload. */
    public val sourceUrl: String? = null,
    public val poster: me.lakko.app.media.PosterFrame? = null,
) {
    /** What the pending bubble draws. */
    public fun preview(): LocalMediaPreview = LocalMediaPreview(
        kind = kind,
        mime = mime,
        name = name,
        durationMs = durationMs,
        waveform = waveform,
        width = width,
        height = height,
        posterBytes = poster?.bytes,
    )
}

/** [LocalMediaPreview.kind] of a message whose only content is tray files
 *  still to upload: its pending bubble shows the first one's thumbnail. */
public const val TRAY_PENDING_KIND: String = "tray"

/**
 * docs/ux/media-messages.md section 11 — a tray item whose upload had not
 * finished when Send was pressed, handed to the repository with its prepared
 * bytes; [index] is its place among the message's attachments.
 */
public class PendingAttachment(
    public val index: Int,
    public val bytes: ByteArray,
    public val mime: String?,
    public val name: String?,
    public val width: Int? = null,
    public val height: Int? = null,
    public val thumbBytes: ByteArray? = null,
    public val thumbWidth: Int? = null,
    public val thumbHeight: Int? = null,
    public val inlineThumb: ByteArray? = null,
    public val audio: Boolean = false,
    public val audioDurationMs: Long? = null,
    public val title: String? = null,
    public val artist: String? = null,
) {
    /** The entry part and the files it is stored as (file id to bytes). */
    public fun toOutbox(newId: () -> String = { java.util.UUID.randomUUID().toString() }): Pair<OutboxFile, Map<String, ByteArray>> {
        val fileId = newId()
        val thumbId = thumbBytes?.let { newId() }
        val stored = HashMap<String, ByteArray>()
        stored[fileId] = bytes
        if (thumbId != null && thumbBytes != null) stored[thumbId] = thumbBytes
        return OutboxFile(
            index = index,
            fileId = fileId,
            mime = mime,
            name = name,
            width = width,
            height = height,
            thumbFileId = thumbId,
            thumbWidth = thumbWidth,
            thumbHeight = thumbHeight,
            inlineThumbB64 = inlineThumb?.let { java.util.Base64.getEncoder().encodeToString(it) },
            audio = audio,
            audioDurationMs = audioDurationMs,
            title = title,
            artist = artist,
        ) to stored
    }
}
