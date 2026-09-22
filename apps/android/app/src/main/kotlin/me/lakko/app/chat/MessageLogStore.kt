// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import android.content.Context
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
import java.io.File
import java.security.KeyStore
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

/**
 * Persistent plaintext-message log on Android (Phase 2a3f1).
 *
 * Mirror of:
 *
 *   * `apps/ios/Lakko/Chat/MessageLogStore.swift`
 *   * `apps/web/src/chat/message-log-store.ts` (forthcoming)
 *
 * Persists the per-account `{ groupIdHex → (cursor, messages) }`
 * map the in-memory [ChatRepository] keeps in
 * [GroupRecord]. Without this layer the timeline vanishes on
 * every app launch — MLS state survives via
 * [IdentitySnapshotStore] but cannot decrypt already-consumed
 * wire bytes (forward secrecy intentionally wipes chain keys).
 *
 * ## Threat model
 *
 * The stored bytes ARE plaintext messages — text + attachment
 * refs. Wrapped under a Keystore-resident AES-256-GCM key with
 * the same posture as [me.lakko.app.devices.DeviceGroupSnapshotStore]:
 *
 *   * dedicated alias [WRAP_KEY_ALIAS], versioned so a future
 *     rotation has somewhere to go;
 *   * `noBackupFilesDir` so a Google-cloud-backup auto-restore
 *     does NOT carry the timeline across devices.
 *
 * ## On-disk layout
 *
 * ```text
 *   noBackupFilesDir/lakko-message-logs/
 *     <accountIdB64>.bin     12-byte IV | ciphertext | 16-byte tag
 * ```
 *
 * Plaintext is the JSON-encoded [MessageLogPayload]. Format
 * version field (`version: 1`) keeps older blobs decryptable
 * across migrations.
 */
public object MessageLogStore {

    private const val AES_GCM_NOPAD = "AES/GCM/NoPadding"
    private const val GCM_IV_LEN = 12
    private const val GCM_TAG_BITS = 128
    private const val AES_KEY_BITS = 256
    private const val ANDROID_KEYSTORE = "AndroidKeyStore"

    /** Keystore alias for the wrap key. Versioned so a future
     *  rotation has somewhere to go. Distinct from the
     *  MLS-snapshot wrap key + the DeviceGroup-snapshot wrap
     *  key — every store gets its own alias for the same
     *  rotational-independence reason. */
    public const val WRAP_KEY_ALIAS: String = "me.lakko.message-log-wrap.v1"

    /** Subdirectory under the app's no-backup files dir where
     *  per-account message-log envelopes live. */
    private const val ENVELOPE_DIR = "lakko-message-logs"

    /**
     * `ignoreUnknownKeys = true`, and the reason is a RETIRED field rather
     * than a future one.
     *
     * ADR-0023 persisted the author's MLS LEAF (`authorLeaf`) and each
     * reaction's reactor leaf (`reactorLeaf`). Both are gone — a leaf is a
     * position, so a stored one names a different member after the next commit
     * — but they are still written into every envelope on disk today. Under
     * the previous `ignoreUnknownKeys = false` those retired keys would throw,
     * [getLog] would return `null`, and the whole account's transcript would
     * be silently discarded. Skipping them instead is what makes "do not
     * migrate, do not wipe" true: a pre-existing row decodes with the new
     * [PersistedMessage.authorKeyB64] absent, which reads as an unknown author
     * and fails the edit/delete gate closed — exactly the posture every
     * reloaded row had before authorship was persisted at all.
     *
     * Output stays predictable for golden tests (`prettyPrint = false`,
     * `explicitNulls = true`).
     */
    private val json: Json = Json {
        ignoreUnknownKeys = true
        prettyPrint = false
        explicitNulls = true
    }

    /**
     * Persist [payload] for [accountIdB64]. Overwrites any
     * prior envelope. Best-effort — Keystore / FS failures
     * surface as `false` so the caller doesn't crash the
     * mutation flow.
     */
    public fun putLog(
        context: Context,
        accountIdB64: String,
        payload: MessageLogPayload,
    ): Boolean {
        val plaintext = try {
            json.encodeToString(MessageLogPayload.serializer(), payload).toByteArray(Charsets.UTF_8)
        } catch (_: Throwable) {
            return false
        }
        val wrap = try {
            getOrCreateWrapKey()
        } catch (_: Throwable) {
            return false
        }
        return try {
            val cipher = Cipher.getInstance(AES_GCM_NOPAD)
            cipher.init(Cipher.ENCRYPT_MODE, wrap)
            val iv = cipher.iv
            check(iv.size == GCM_IV_LEN) {
                "unexpected GCM IV length from Keystore: ${iv.size}"
            }
            val ciphertext = cipher.doFinal(plaintext)

            val out = ByteArray(GCM_IV_LEN + ciphertext.size)
            System.arraycopy(iv, 0, out, 0, GCM_IV_LEN)
            System.arraycopy(ciphertext, 0, out, GCM_IV_LEN, ciphertext.size)
            envelopeFile(context, accountIdB64).writeBytes(out)
            true
        } catch (_: Throwable) {
            false
        }
    }

    /**
     * Read + unwrap the log for [accountIdB64]. Returns `null`
     * when:
     *   * no envelope on disk for this account,
     *   * Keystore access denied / wrap key rotated since the
     *     envelope was written,
     *   * truncated / non-JSON blob (corrupt — caller starts
     *     with an empty timeline and re-bootstraps from the DS
     *     journal cursor).
     */
    public fun getLog(context: Context, accountIdB64: String): MessageLogPayload? {
        val file = envelopeFile(context, accountIdB64)
        if (!file.exists()) return null
        val blob = try {
            file.readBytes()
        } catch (_: Throwable) {
            return null
        }
        if (blob.size <= GCM_IV_LEN) return null
        val iv = blob.copyOfRange(0, GCM_IV_LEN)
        val ciphertext = blob.copyOfRange(GCM_IV_LEN, blob.size)

        val wrap = try {
            getOrCreateWrapKey()
        } catch (_: Throwable) {
            return null
        }
        val plaintext = try {
            val cipher = Cipher.getInstance(AES_GCM_NOPAD)
            cipher.init(Cipher.DECRYPT_MODE, wrap, GCMParameterSpec(GCM_TAG_BITS, iv))
            cipher.doFinal(ciphertext)
        } catch (_: Throwable) {
            return null
        }
        return decodeLogPayload(String(plaintext, Charsets.UTF_8))
    }

    /**
     * The pure JSON half of [getLog]: `null` on anything that will not decode.
     *
     * Split out of [getLog] for the same reason [withoutGroup] is split out of
     * [deleteGroupFromLog] — the host suite has no Keystore and no `Context`,
     * and this is where the rule that could actually be wrong lives. What it
     * pins is the retired-key behaviour above: an envelope written by the
     * pre-fix build carries `authorLeaf` / `reactorLeaf`, and this must still
     * hand back the transcript rather than `null`.
     */
    internal fun decodeLogPayload(plaintext: String): MessageLogPayload? =
        try {
            json.decodeFromString(MessageLogPayload.serializer(), plaintext)
        } catch (_: Throwable) {
            null
        }

    /**
     * Erase the log for [accountIdB64]. No-op when absent.
     * Called from `forgetDevice`. The Keystore wrap key is
     * left intact — shared across accounts on the same
     * device, rotating it would invalidate every surviving
     * envelope.
     */
    public fun deleteLog(context: Context, accountIdB64: String) {
        envelopeFile(context, accountIdB64).delete()
    }

    /**
     * ADR-0045 UX §6.2.4 — drop ONE group's transcript from this device.
     *
     * No client had a per-group history delete before: [deleteLog] and its
     * `IdentitySnapshotStore` sibling are whole-ACCOUNT operations. But the log
     * is a per-account blob keyed `{ groupIdHex -> PersistedGroup }`
     * internally, so dropping one group's entry is a read-modify-write on a
     * store that already exists rather than a new one.
     *
     * Returns `false` when there was nothing to drop (no envelope, an
     * undecryptable one, or no entry for this group), so the caller can tell
     * "deleted" from "there was nothing there" without a second read — and can
     * skip the re-encrypt + write entirely.
     *
     * **What this does NOT remove, stated because the confirmation copy
     * ([DELETE_CHAT_BODY]) is exact rather than sweeping:** the inactive MLS
     * group stays in the provider snapshot as inert bytes. `Group` exposes no
     * delete, so a `Group::delete` is a real (small) core follow-up rather than
     * something this rides free. The remnant is inert in the strong sense — it
     * cannot send, decrypt, export or process — but it is not nothing.
     */
    public fun deleteGroupFromLog(
        context: Context,
        accountIdB64: String,
        groupIdHex: String,
    ): Boolean {
        val payload = getLog(context, accountIdB64) ?: return false
        val next = withoutGroup(payload, groupIdHex) ?: return false
        return putLog(context, accountIdB64, next)
    }

    // -----------------------------------------------------------------
    // Internal
    // -----------------------------------------------------------------

    private fun getOrCreateWrapKey(): SecretKey {
        val ks = KeyStore.getInstance(ANDROID_KEYSTORE).apply { load(null) }
        ks.getKey(WRAP_KEY_ALIAS, null)?.let { existing ->
            return existing as SecretKey
        }
        val builder = KeyGenParameterSpec.Builder(
            WRAP_KEY_ALIAS,
            KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT,
        )
            .setKeySize(AES_KEY_BITS)
            .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
            .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
            .setRandomizedEncryptionRequired(true)
            .setUserAuthenticationRequired(false)
        val kg = KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, ANDROID_KEYSTORE)
        kg.init(builder.build())
        return kg.generateKey()
    }

    private fun envelopeFile(context: Context, accountIdB64: String): File {
        val dir = File(context.noBackupFilesDir, ENVELOPE_DIR)
        if (!dir.exists()) dir.mkdirs()
        val safe = accountIdB64.replace("/", "_")
        return File(dir, "$safe.bin")
    }
}

// =============================================================================
// JSON payload — mirror of the iOS PersistedMessage / PersistedAttachment shapes
// =============================================================================

@Serializable
public data class MessageLogPayload(
    val version: Int = 1,
    val groups: Map<String, PersistedGroup>,
)

@Serializable
public data class PersistedGroup(
    val cursor: Long,
    val messages: List<PersistedMessage>,
)

/**
 * ADR-0045 UX §6.2.4 — the pure half of [MessageLogStore.deleteGroupFromLog].
 *
 * Kept separate from the IO so it is testable: the host suite has no Keystore
 * and no `Context`, so [MessageLogStore.deleteGroupFromLog] cannot be, and this
 * is where the rule that could actually be wrong lives.
 *
 * Returns `null` when [groupIdHex] was **absent**, so the caller can skip the
 * re-encrypt + write entirely. Never mutates its input.
 */
public fun withoutGroup(
    payload: MessageLogPayload,
    groupIdHex: String,
): MessageLogPayload? {
    if (!payload.groups.containsKey(groupIdHex)) return null
    return payload.copy(groups = payload.groups.filterKeys { it != groupIdHex })
}

@Serializable
public data class PersistedMessage(
    val messageId: Long,
    val direction: String,
    val text: String,
    val attachments: List<PersistedAttachment>,
    val createdAt: String,
    /**
     * ADR-0023 — the author's IDENTITY: their Ed25519 signature public key,
     * base64, resolved from the MLS sender leaf at receipt (see
     * `AuthorIdentity.kt`). `null` when the client could not resolve it, and
     * **absent entirely on a row written before this field existed** — both
     * read back as "unknown author", which fails the edit/delete gate closed.
     * That is the same posture every reloaded message had before authorship
     * was persisted; it never misattributes.
     *
     * This REPLACES the author leaf, which is not merely unhelpful here but
     * actively dangerous: persisting a leaf is what made the leaf-reuse
     * impersonation survive a restart on Android, for the whole retained
     * transcript. The retired key is skipped on decode, never round-tripped —
     * a stored leaf that still decoded cleanly is exactly what a later reader
     * would reach for.
     *
     * No secret is added to the envelope: the key is public, and already sits
     * in the MLS snapshot store beside it.
     */
    val authorKeyB64: String? = null,
    /** ADR-0023 — `true` once an edit op replaced the text. */
    val edited: Boolean = false,
    /** ADR-0023 — `true` once a delete op tombstoned the row. */
    val deleted: Boolean = false,
    /** ADR-0023 — accumulated reactions. */
    val reactions: List<PersistedReaction> = emptyList(),
    /**
     * `docs/ux/calls.md` section 6 — the finished call this row records, when it
     * records one.
     *
     * Nullable with a default, so a transcript written before call rows existed
     * decodes unchanged and a client that predates them skips the key (the
     * store already decodes with `ignoreUnknownKeys`). This is what makes a call
     * row survive a relaunch the way a message does: no new store, no
     * migration, the same encrypted per-account envelope.
     */
    val call: PersistedCall? = null,
) {
    /** Explicit companion so the conversion helpers below can
     *  attach as `PersistedMessage.Companion.from(msg)`. */
    public companion object
}

/**
 * `docs/ux/calls.md` section 6 — a finished call as the transcript stores it.
 *
 * The row's TEXT is stored beside this in [PersistedMessage.text] and is what
 * renders if the marker ever fails to decode, so a future outcome name costs a
 * plain pill rather than a lost row.
 */
@Serializable
public data class PersistedCall(
    /** Lowercase name of a [me.lakko.app.calls.CallOutcome]. */
    val outcome: String,
    val video: Boolean = false,
    val durationSecs: Long? = null,
)

@Serializable
public data class PersistedReaction(
    /**
     * ADR-0023 — the reactor's signature key, base64. Nullable WITH A DEFAULT
     * so a row written before this field existed still decodes (its retired
     * `reactorLeaf` is skipped); such a reaction is DROPPED on restore rather
     * than restored under a guessed reactor, because an unattributed reaction
     * cannot be toggled off by anybody and would merge with the next one.
     */
    val reactorKeyB64: String? = null,
    val emoji: String,
)

@Serializable
public data class PersistedAttachment(
    val blobIdB64: String,
    val keyB64: String,
    val nonceB64: String,
    val mime: String?,
    val size: ULong,
    val name: String?,
    /**
     * ADR-0047 — the media metadata. Every field defaults to `null`, so a row
     * written before these existed decodes as a plain file (the posture an old
     * client already renders), and a row written now round-trips the bubble
     * shape across a restart. The bounds are the core's; what is stored is
     * what the core handed over, so nothing out of bound is ever written.
     *
     * Device-history transfer is a different path: the `lakko-mls` history
     * placeholder codec does not carry these fields yet, so a media message
     * moved to a new device arrives as a plain file. Known and deferred; no
     * workaround here.
     */
    val kind: String? = null,
    val durationMs: UInt? = null,
    val waveformB64: String? = null,
    val width: UInt? = null,
    val height: UInt? = null,
    val thumb: PersistedThumb? = null,
) {
    public companion object
}

/** ADR-0047 — a video-note poster blob reference, persisted beside its note. */
@Serializable
public data class PersistedThumb(
    val blobIdB64: String,
    val keyB64: String,
    val nonceB64: String,
    val mime: String?,
    val size: ULong,
    val width: UInt? = null,
    val height: UInt? = null,
)

// =============================================================================
// ChatMessage ↔ PersistedMessage helpers
// =============================================================================

/** Build the JSON projection from a live in-memory [ChatMessage]. */
public fun PersistedMessage.Companion.from(msg: ChatMessage): PersistedMessage =
    PersistedMessage(
        messageId = msg.messageId,
        direction = when (msg.direction) {
            Direction.Outgoing -> "outgoing"
            Direction.Incoming -> "incoming"
            Direction.System -> "system"
        },
        // ADR-0023 — a deleted message is stored EMPTY. The bubble and the
        // conversation row both draw the tombstone from `deleted` alone, so
        // nothing reads either of these again; keeping them would write the
        // words of a message someone chose to delete - and the content keys
        // of its attachments, which open the blobs on the server - to this
        // device for as long as the transcript lives, in exchange for
        // nothing. The web client stores it the same way.
        text = if (msg.deleted) "" else msg.text,
        attachments =
            if (msg.deleted) emptyList() else msg.attachments.map(PersistedAttachment.Companion::from),
        createdAt = msg.createdAt,
        // ADR-0023 — the identity we resolved at receipt (or stamped at send),
        // written back verbatim: the store is the record of who wrote this, not
        // a place to re-derive it from whoever is holding the device now.
        authorKeyB64 = msg.authorKey,
        edited = msg.edited,
        deleted = msg.deleted,
        reactions = msg.reactions.map {
            PersistedReaction(reactorKeyB64 = it.reactor, emoji = it.emoji)
        },
        call = msg.call?.let {
            PersistedCall(
                outcome = it.outcome.name.lowercase(java.util.Locale.ROOT),
                video = it.video,
                durationSecs = it.durationSecs,
            )
        },
    )

/** Inverse — reconstruct a runtime [ChatMessage] from the JSON
 *  projection. Returns `null` on a malformed `direction`
 *  discriminator (older client parsing a future variant). */
public fun PersistedMessage.toChatMessage(): ChatMessage? {
    val dir = when (direction) {
        "outgoing" -> Direction.Outgoing
        "incoming" -> Direction.Incoming
        "system" -> Direction.System
        else -> return null
    }
    return ChatMessage(
        messageId = messageId,
        direction = dir,
        text = text,
        attachments = attachments.mapNotNull { it.toAttachmentRef() },
        createdAt = createdAt,
        // Absent on a row written before this field existed, and `null` when it
        // could not be resolved — both restore as `null`, which fails the
        // edit/delete gate closed rather than falling back to whoever is
        // holding the device now. That fallback would invent an author the
        // store never recorded.
        authorKey = authorKeyB64,
        edited = edited,
        deleted = deleted,
        reactions = reactions.mapNotNull { r ->
            r.reactorKeyB64?.let { Reaction(reactor = it, emoji = r.emoji) }
        },
        // An outcome this build does not know reads back as `null`: the row
        // survives and renders as the plain system pill its stored text already
        // is, rather than being dropped or guessed at.
        call = call?.toTranscriptEntry(),
    )
}

/** `null` for an unrecognised outcome — see [PersistedMessage.toChatMessage]. */
internal fun PersistedCall.toTranscriptEntry(): me.lakko.app.calls.CallTranscriptEntry? {
    val parsed = me.lakko.app.calls.CallOutcome.entries.firstOrNull {
        it.name.equals(outcome, ignoreCase = true)
    } ?: return null
    return me.lakko.app.calls.CallTranscriptEntry(
        outcome = parsed,
        video = video,
        durationSecs = durationSecs?.takeIf { it >= 0L },
    )
}

// `java.util.Base64` rather than `android.util.Base64`: the same standard
// alphabet with padding and no line wrapping as the `NO_WRAP` flag this used to
// pass (so every stored row still decodes), available from API 26 (this app's
// minimum), and it runs on the host JVM, which is what lets the round trip be
// unit-tested.
private val b64Encoder = java.util.Base64.getEncoder()
private val b64Decoder = java.util.Base64.getDecoder()

public fun PersistedAttachment.Companion.from(
    ref: uniffi.lakko_ffi_uniffi.AttachmentRef,
): PersistedAttachment = PersistedAttachment(
    blobIdB64 = b64Encoder.encodeToString(ref.blobId),
    keyB64 = b64Encoder.encodeToString(ref.key),
    nonceB64 = b64Encoder.encodeToString(ref.nonce),
    mime = ref.mime,
    size = ref.size,
    name = ref.name,
    kind = ref.kind,
    durationMs = ref.durationMs,
    waveformB64 = ref.waveform?.let(b64Encoder::encodeToString),
    width = ref.width,
    height = ref.height,
    thumb = ref.thumb?.let { t ->
        PersistedThumb(
            blobIdB64 = b64Encoder.encodeToString(t.blobId),
            keyB64 = b64Encoder.encodeToString(t.key),
            nonceB64 = b64Encoder.encodeToString(t.nonce),
            mime = t.mime,
            size = t.size,
            width = t.width,
            height = t.height,
        )
    },
)

/**
 * Inverse of [PersistedAttachment.Companion.from]. `null` when a REQUIRED
 * field (blob id, key, nonce) does not decode — the attachment cannot be
 * fetched without them. A malformed OPTIONAL field (a waveform or poster that
 * does not decode) is dropped and the attachment survives, the same rule the
 * core applies to the wire (ADR-0047 §1).
 */
public fun PersistedAttachment.toAttachmentRef(): uniffi.lakko_ffi_uniffi.AttachmentRef? {
    return try {
        uniffi.lakko_ffi_uniffi.AttachmentRef(
            blobId = b64Decoder.decode(blobIdB64),
            key = b64Decoder.decode(keyB64),
            nonce = b64Decoder.decode(nonceB64),
            mime = mime,
            size = size,
            name = name,
            kind = kind,
            durationMs = durationMs,
            waveform = waveformB64?.let { runCatching { b64Decoder.decode(it) }.getOrNull() },
            width = width,
            height = height,
            thumb = thumb?.let { t ->
                runCatching {
                    uniffi.lakko_ffi_uniffi.ThumbRef(
                        blobId = b64Decoder.decode(t.blobIdB64),
                        key = b64Decoder.decode(t.keyB64),
                        nonce = b64Decoder.decode(t.nonceB64),
                        mime = t.mime,
                        size = t.size,
                        width = t.width,
                        height = t.height,
                    )
                }.getOrNull()
            },
        )
    } catch (_: IllegalArgumentException) {
        null
    }
}
