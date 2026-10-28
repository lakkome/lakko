// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.profile

import kotlinx.serialization.Serializable
import uniffi.lakko_ffi_uniffi.FfiAvatar
import uniffi.lakko_ffi_uniffi.FfiAvatarRef

/*
 * ADR-0084 — the profile photo as this client keeps it: the encrypted blob
 * references a profile carries (a still, and for a profile video the video
 * beside it), in a form that serialises into the own-profile and the
 * peer-profile stores. The bytes themselves live in storage; the decrypted
 * copies live in [me.lakko.app.avatar.AvatarCache].
 */

/** One encrypted avatar blob: what `encryptAttachment` returned for it, its
 *  plaintext size and its side in pixels (the picture is square). */
@Serializable
public data class AvatarBlob(
    /** 32-byte blob id, lowercase hex (`GET /blob/<hex>`). */
    val blobIdHex: String,
    /** 32-byte AEAD key, standard base64. SECRET. */
    val keyB64: String,
    /** 12-byte AEAD nonce, standard base64. */
    val nonceB64: String,
    /** Plaintext size, bytes. */
    val size: Long,
    /** Side, pixels. */
    val side: Int,
) {
    override fun toString(): String = "AvatarBlob($blobIdHex, size=$size, side=$side)"
}

/** A profile's avatar: the still, and optionally the video it is the first
 *  frame of. */
@Serializable
public data class AvatarRefs(
    val still: AvatarBlob,
    val video: AvatarBlob? = null,
    /** The video's length, milliseconds; set exactly when [video] is. */
    val videoDurationMs: Int? = null,
) {
    /** `true` for a profile video. */
    val isVideo: Boolean get() = video != null && videoDurationMs != null
}

/** ADR-0084 §2 bounds, mirrored for the encoder side (the core refuses the
 *  whole envelope outside them; this side never produces such a value). */
public object AvatarBounds {
    public const val STILL_MAX_SIDE: Int = 640
    public const val STILL_MAX_BYTES: Int = 262_144
    public const val VIDEO_MAX_SIDE: Int = 480
    public const val VIDEO_MAX_BYTES: Int = 4_194_304
    public const val VIDEO_MAX_MS: Int = 10_000
}

private fun hex(bytes: ByteArray): String = bytes.joinToString("") { "%02x".format(it.toInt() and 0xff) }

private fun unhex(s: String): ByteArray? {
    if (s.length % 2 != 0) return null
    return try {
        ByteArray(s.length / 2) { i -> s.substring(i * 2, i * 2 + 2).toInt(16).toByte() }
    } catch (_: NumberFormatException) {
        null
    }
}

/** The raw blob id, or `null` when [AvatarBlob.blobIdHex] is not 32-byte hex. */
public fun AvatarBlob.blobId(): ByteArray? = unhex(blobIdHex)?.takeIf { it.size == 32 }

/** The raw key, or `null`. SECRET. */
public fun AvatarBlob.key(): ByteArray? = try {
    java.util.Base64.getDecoder().decode(keyB64).takeIf { it.size == 32 }
} catch (_: IllegalArgumentException) {
    null
}

/** The raw nonce, or `null`. */
public fun AvatarBlob.nonce(): ByteArray? = try {
    java.util.Base64.getDecoder().decode(nonceB64).takeIf { it.size == 12 }
} catch (_: IllegalArgumentException) {
    null
}

/** An [AvatarBlob] from the raw parts. */
public fun avatarBlobOf(blobId: ByteArray, key: ByteArray, nonce: ByteArray, size: Long, side: Int): AvatarBlob =
    AvatarBlob(
        blobIdHex = hex(blobId),
        keyB64 = java.util.Base64.getEncoder().encodeToString(key),
        nonceB64 = java.util.Base64.getEncoder().encodeToString(nonce),
        size = size,
        side = side,
    )

private fun AvatarBlob.toFfi(): FfiAvatarRef? {
    val id = blobId() ?: return null
    val k = key() ?: return null
    val n = nonce() ?: return null
    if (size !in 1..UInt.MAX_VALUE.toLong() || side !in 1..UShort.MAX_VALUE.toInt()) return null
    return FfiAvatarRef(blobId = id, key = k, nonce = n, size = size.toUInt(), side = side.toUShort())
}

/** The wire form, or `null` when a stored part is malformed (the profile
 *  then goes out without a photo rather than not at all). */
public fun AvatarRefs.toFfi(): FfiAvatar? {
    val still = still.toFfi() ?: return null
    val v = video
    val d = videoDurationMs
    if (v == null || d == null) return FfiAvatar(still = still, video = null, videoDurationMs = null)
    val video = v.toFfi() ?: return FfiAvatar(still = still, video = null, videoDurationMs = null)
    if (d !in 1..AvatarBounds.VIDEO_MAX_MS) return FfiAvatar(still = still, video = null, videoDurationMs = null)
    return FfiAvatar(still = still, video = video, videoDurationMs = d.toUShort())
}

private fun FfiAvatarRef.toBlob(): AvatarBlob =
    avatarBlobOf(blobId, key, nonce, size.toLong(), side.toInt())

/** What a decoded profile's avatar is kept as. */
public fun avatarRefsOf(avatar: FfiAvatar?): AvatarRefs? {
    val a = avatar ?: return null
    val video = a.video
    val duration = a.videoDurationMs
    return AvatarRefs(
        still = a.still.toBlob(),
        video = if (video != null && duration != null) video.toBlob() else null,
        videoDurationMs = if (video != null && duration != null) duration.toInt() else null,
    )
}

/** ADR-0065 section 6 — the blob ids of [previous] that [next] no longer
 *  names (a replaced or removed photo), to delete from the server. */
public fun replacedAvatarBlobs(previous: AvatarRefs?, next: AvatarRefs?): List<ByteArray> {
    val old = listOfNotNull(previous?.still, previous?.video)
    val keep = listOfNotNull(next?.still, next?.video).map { it.blobIdHex }.toSet()
    return old.filter { it.blobIdHex !in keep }.mapNotNull { it.blobId() }
}

/** ADR-0065 section 3 — the blob ids of the own current photo, for the keep
 *  tick. */
public fun avatarBlobIds(refs: AvatarRefs?): List<ByteArray> =
    listOfNotNull(refs?.still, refs?.video).mapNotNull { it.blobId() }
