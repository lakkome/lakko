// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.push

/**
 * Read-only THROWAWAY MLS decrypt for the FCM silent-push path.
 *
 * The Android counterpart of the web `public/sw-push-decrypt.js`
 * (commit 148753b). When an FCM data-message wake lands and the app
 * is NOT foregrounded, [me.lakko.app.push.LakkoMessagingService]
 * builds the real (stores / FFI / DS) dependency object and calls
 * [decryptPushPreview] to turn the `(group_pseudonym, pointer)` wake
 * hint into the REAL decrypted text of the newest message, so the
 * notification body says what actually arrived instead of a
 * content-free "New message".
 *
 * ## Why this file holds no Android / FFI types
 *
 * Every capability it needs — the store reads, the MLS restore +
 * decrypt, the DS fetch — is INJECTED via [PushDecryptDeps]. The file
 * imports only the Kotlin stdlib, so it loads cleanly in a plain JVM
 * unit test (`app/src/test/...`) where neither the Android framework
 * (Keystore, Context) nor the UniFFI native `.so` is available. The
 * production wiring that touches those lives in
 * `PushBackgroundDecrypt.kt`; the JUnit test drives this core with
 * hand-rolled fakes.
 *
 * ## Load-bearing correctness: READ-ONLY THROWAWAY
 *
 * This path MUST NOT corrupt the MLS ratchet state the foreground app
 * relies on (the FMS runs in the SAME process as the app). It:
 *
 *   1. reads the AccountIdentity seed + the last-persisted MLS
 *      snapshot (both READ-ONLY),
 *   2. restores a TEMPORARY, in-memory [ThrowawayGroup] from that
 *      snapshot (a fresh `Identity` + `Group` — NEVER the app's live
 *      handles),
 *   3. fetches the DS journal after a READ-ONLY cursor and processes
 *      the rows through the temp group ONLY to extract the newest
 *      message's plaintext preview,
 *   4. returns the preview and DISCARDS the temp group (`close()`).
 *
 * It NEVER persists the advanced snapshot, NEVER advances the app's DS
 * cursor, NEVER writes any MLS state. [PushDecryptDeps] deliberately
 * exposes NO write capability — the type system itself forbids
 * persisting. Were the FMS to persist the advanced state, the
 * foreground app — which restores from the SAME last-persisted
 * snapshot and does the real durable processing from its OWN cursor —
 * would desync its ratchet (the G-34 nonce-reuse-on-crash class). The
 * DS journal is append-only + cursor-based, so the app re-processing
 * from its own cursor on next foreground is idempotent.
 *
 * ## Fail-closed (security invariant #4)
 *
 * A frame that will not decrypt / authenticate is DROPPED, never shown
 * as a plaintext guess ([ThrowawayGroup.process] returns `null`). Any
 * missing input (no account match, no seed, no snapshot, non-member
 * group, DS failure, zero decryptable frames) returns `null`, and the
 * FMS shows the generic notification.
 *
 * ## Scope
 *
 * GroupMessage wakes only (chat / edit / attachment-hint text). A
 * Wakeup / MailboxMessage wake (no groupHex / no pointer) returns
 * `null` here and stays generic (a follow-up), mirroring the web
 * slice.
 */

/** Max characters of decrypted text shown in the notification body. */
private const val PREVIEW_MAX_CHARS = 140

/** Length in hex chars of a 32-byte group_id. */
private const val GROUP_HEX_LEN = 64

/**
 * The GroupMessage wake hint, already decoded from the FCM `data`
 * payload (`{ v, d, g, p }`). Only `g` + `p` (DS pointer) are
 * load-bearing for the preview.
 */
public data class PushWake(
    /** 64 lowercase-hex chars — post-ADR-0042 the opaque 32-byte
     *  `push_gid` (reverse-resolved via [PushDecryptDeps.lookupRoute]);
     *  a pre-ADR-0042 legacy wake carries the raw 32-byte group_id.
     *  `null` for a non-group wake. */
    val groupHex: String?,
    /** DS message id of the message that triggered the wake, or `null`. */
    val pointer: Long?,
)

/** A locally-stored account that owns the wake's group. */
public data class ResolvedPushAccount(
    /** The account key shared by every per-account store (the same
     *  `Base64.getEncoder()` string `KnownGroupsStore` /
     *  `AccountIdentitySeedStore` / `IdentitySnapshotStore` /
     *  `MessageLogStore` all key on). */
    val accountIdB64: String,
    /** The raw 32-byte group_id (hex-decoded from the wake's `g`). */
    val groupIdBytes: ByteArray,
) {
    override fun equals(other: Any?): Boolean {
        if (this === other) return true
        if (other !is ResolvedPushAccount) return false
        return accountIdB64 == other.accountIdB64 &&
            groupIdBytes.contentEquals(other.groupIdBytes)
    }

    override fun hashCode(): Int =
        31 * accountIdB64.hashCode() + groupIdBytes.contentHashCode()
}

/** One DS journal row (server id + raw MLS wire bytes). */
public data class FetchedRow(
    val id: Long,
    val payload: ByteArray,
) {
    override fun equals(other: Any?): Boolean {
        if (this === other) return true
        if (other !is FetchedRow) return false
        return id == other.id && payload.contentEquals(other.payload)
    }

    override fun hashCode(): Int = 31 * id.hashCode() + payload.contentHashCode()
}

/** Classification of a decrypted application message (mirror of the
 *  UniFFI `MessageBodyKind`), kept native-free so it can be produced by
 *  the fake in the unit test. */
public enum class ProcessedKind { CHAT, EDIT, DELETE, REACTION, TYPING, READ_RECEIPT }

/** A decoded application row (post MLS-decrypt + envelope-parse). */
public data class ProcessedRow(
    val kind: ProcessedKind,
    /** Chat / edit text (empty for the other kinds). */
    val text: String,
    /** Number of attachment refs (chat only). */
    val attachmentCount: Int,
    /** ADR-0048 — the sender's name, when this device ALREADY knows one
     *  better than a fingerprint (read from the stored profiles; never
     *  fetched or verified here). `null` otherwise. */
    val senderName: String? = null,
)

/** The notification preview the FMS renders when the decrypt succeeds. */
public data class PushPreview(
    val title: String,
    val body: String,
    /** Notification tag (the group hex — collapses a burst into one
     *  OS notification). */
    val tag: String,
)

/**
 * A TEMPORARY, in-memory MLS group restored from the last-persisted
 * snapshot. The production implementation wraps a throwaway `Identity`
 * + `Group`; [close] frees them and MUST NEVER persist / snapshot them
 * back (read-only-throwaway discipline).
 */
public interface ThrowawayGroup : AutoCloseable {
    /**
     * Process ONE wire row through the throwaway group. Returns the
     * decoded row, or `null` when the frame does not decrypt /
     * authenticate, is our own echo, or is a control frame
     * (fail-closed skip — never a plaintext guess). MUST NOT persist
     * any MLS state.
     */
    public fun process(payload: ByteArray): ProcessedRow?
}

/**
 * READ-ONLY injected capabilities. Every method is a read or a pure
 * compute — there is deliberately NO write path (no `saveSnapshot` /
 * `advanceCursor`). The interface simply cannot persist, which is the
 * structural enforcement of the read-only-throwaway discipline.
 */
public interface PushDecryptDeps {
    /** ADR-0042 (Sprint 4): reverse-map an opaque wake `g` (the
     *  lowercase-hex `push_gid`) to its owning
     *  `(accountIdB64, groupIdHex)` via the plaintext reverse index
     *  Sprint 3 populates ([PushRouteStore.lookup]), or `null` when
     *  unknown. A READ — never a write. */
    public fun lookupRoute(pushGidHex: String): Pair<String, String>?

    /** Match `groupHex` to a locally-stored account+group. `null` when
     *  no account on this device knows the group (a stray wake). */
    public fun resolveAccount(groupHex: String): ResolvedPushAccount?

    /** The seed the snapshot's identity is rebuilt from for `accountIdB64`,
     *  or `null`. Since ADR-0050 this is the per-device conversation seed
     *  (the snapshot is written under the device identity). */
    public fun loadIdentitySeed(accountIdB64: String): ByteArray?

    /** The last-persisted MLS provider snapshot for `accountIdB64`, or
     *  `null`. Only ever READ — never written back. */
    public fun loadSnapshot(accountIdB64: String): ByteArray?

    /** Build a THROWAWAY `Identity` + `Group` from the snapshot.
     *  `null` when the account is not a member of the group / the
     *  restore fails. */
    public fun openThrowawayGroup(
        seed: ByteArray,
        snapshot: ByteArray,
        groupIdBytes: ByteArray,
    ): ThrowawayGroup?

    /** READ-ONLY DS group-journal fetch of rows strictly after
     *  `afterId` (ascending id order). */
    public suspend fun dsFetch(groupIdBytes: ByteArray, afterId: Long): List<FetchedRow>

    /** The app's last-persisted per-group cursor (READ-ONLY), or
     *  `null`. Used as the fetch start so intervening commits replay
     *  and the epoch stays correct. */
    public fun startCursor(accountIdB64: String, groupHex: String): Long?
}

/**
 * Produce a real decrypted notification preview for a GroupMessage
 * wake, or `null` when the background path cannot (safely) yield one —
 * in which case the FMS shows its generic "New message" fallback.
 *
 * Never throws for a "cannot decrypt" reason; every such path returns
 * `null`. (A programming error in a mock could still throw in tests —
 * the FMS wraps the whole call in try/catch regardless.)
 */
public suspend fun decryptPushPreview(
    wake: PushWake?,
    deps: PushDecryptDeps,
): PushPreview? {
    // 1. Only GroupMessage wakes carry a 64-hex group + a pointer. A
    //    Wakeup / MailboxMessage wake is not handled in this slice.
    val rawGroupHex = wake?.groupHex ?: return null
    val pointer = wake.pointer ?: return null
    val groupHex = rawGroupHex.lowercase()
    if (groupHex.length != GROUP_HEX_LEN || !isLowerHex(groupHex)) return null

    // 2. Resolve the wake's `g` to a locally-stored account + the REAL
    //    DS group_id. ADR-0042 (Sprint 4): post-Sprint-3, notify sends
    //    `g = hex(push_gid)` — an opaque routing id, NOT the group_id —
    //    so FIRST reverse-resolve it via the plaintext route index; a
    //    hit yields the owning account + the real group_id used for
    //    everything downstream (snapshot, loadGroup, DS fetch, cursor).
    //    A miss, a lookup error, or a malformed stored row FALLS BACK
    //    to the legacy raw-group_id path (pre-ADR-0042 wakes), which
    //    itself fails closed to `null` (→ generic) on a stray wake.
    var account: ResolvedPushAccount? = null
    var realGroupHex = groupHex
    val route = try {
        deps.lookupRoute(groupHex)
    } catch (_: Throwable) {
        null
    }
    if (route != null) {
        val routedHex = route.second.lowercase()
        val routedBytes =
            if (routedHex.length == GROUP_HEX_LEN && isLowerHex(routedHex)) {
                hexToBytes(routedHex)
            } else {
                null
            }
        if (routedBytes != null) {
            account = ResolvedPushAccount(
                accountIdB64 = route.first,
                groupIdBytes = routedBytes,
            )
            realGroupHex = routedHex
        }
    }
    if (account == null) {
        // Legacy path: treat `g` as the raw group_id (also validates
        // the wake's group is one we actually know — a stray wake →
        // null).
        account = deps.resolveAccount(groupHex) ?: return null
        realGroupHex = groupHex
    }

    // 3. The AccountIdentity seed → the throwaway Identity.
    val seed = deps.loadIdentitySeed(account.accountIdB64) ?: return null

    // 4. The last-persisted MLS provider snapshot (the ratchet state
    //    the foreground app relies on — we only READ it).
    val snapshot = deps.loadSnapshot(account.accountIdB64) ?: return null

    // 5. Build a TEMPORARY, in-memory group from the snapshot.
    //    Everything from here is throwaway — never snapshotted back.
    val group = try {
        deps.openThrowawayGroup(seed, snapshot, account.groupIdBytes)
    } catch (_: Throwable) {
        null
    } finally {
        // The throwaway Identity has copied the seed; zero our view of
        // it (mirrors the seed-store caller contract).
        seed.fill(0)
    } ?: return null

    try {
        // 6. Fetch DS rows after a READ-ONLY start cursor. Prefer the
        //    app's persisted per-group cursor (so intervening commits
        //    since the snapshot are replayed and the epoch stays
        //    correct); otherwise just the message that triggered the
        //    wake (`pointer - 1`).
        var after = pointer - 1
        val persisted = try {
            deps.startCursor(account.accountIdB64, realGroupHex)
        } catch (_: Throwable) {
            null
        }
        if (persisted != null && persisted < pointer) {
            after = persisted
        }
        if (after < 0) after = 0

        val rows = try {
            deps.dsFetch(account.groupIdBytes, after)
        } catch (_: Throwable) {
            return null
        }

        // 7. Process each row through the TEMP group and keep the
        //    NEWEST frame that yields a user-visible preview. A frame
        //    that will not decrypt / authenticate (wrong epoch, our own
        //    echo, a control frame, a role-refused commit) is skipped —
        //    never guessed. NOTE: `process` mutates the THROWAWAY
        //    provider ONLY; it is never persisted.
        var latest: PushPreview? = null
        for (row in rows) {
            val processed = try {
                group.process(row.payload)
            } catch (_: Throwable) {
                continue
            }
            val body = previewBodyOf(processed) ?: continue
            // ADR-0048 UX §5 — the sender's resolved name when it is already
            // known on this device; the generic title otherwise.
            latest = PushPreview(title = processed?.senderName ?: "Lakko", body = body, tag = realGroupHex)
        }
        return latest
    } finally {
        // Discard the throwaway MLS state. We NEVER snapshot it back —
        // doing so would desync the foreground app's ratchet.
        try {
            group.close()
        } catch (_: Throwable) {
            // best-effort
        }
    }
}

/**
 * Extract a user-visible preview string from a processed row, or
 * `null` for anything that is not a renderable chat/edit body (so an
 * older real message stays the preview).
 */
private fun previewBodyOf(row: ProcessedRow?): String? {
    if (row == null) return null
    return when (row.kind) {
        // A normal chat message → its text (or an attachment hint).
        ProcessedKind.CHAT ->
            if (row.text.isNotEmpty()) {
                truncate(row.text)
            } else {
                when {
                    row.attachmentCount == 1 -> "Sent an attachment"
                    row.attachmentCount > 1 -> "Sent ${row.attachmentCount} attachments"
                    else -> null
                }
            }
        // An edit rewrites a prior message → surface the new text.
        ProcessedKind.EDIT ->
            if (row.text.isNotEmpty()) truncate(row.text) else null
        // delete / reaction / typing / read-receipt carry no standalone
        // preview — skip so an older real message stays the preview.
        ProcessedKind.DELETE,
        ProcessedKind.REACTION,
        ProcessedKind.TYPING,
        ProcessedKind.READ_RECEIPT,
        -> null
    }
}

/** Truncate a preview body to a sane notification length. */
private fun truncate(text: String): String =
    if (text.length <= PREVIEW_MAX_CHARS) {
        text
    } else {
        text.take(PREVIEW_MAX_CHARS - 1) + "…"
    }

/** Decode a lowercase even-length hex string into bytes. Returns
 *  `null` on any malformed input (defensive — callers pre-validate
 *  via [isLowerHex]). Kotlin-stdlib-only so the pure core stays
 *  JVM-unit-testable. */
private fun hexToBytes(hex: String): ByteArray? {
    if (hex.length % 2 != 0) return null
    val out = ByteArray(hex.length / 2)
    for (i in out.indices) {
        val hi = Character.digit(hex[i * 2], 16)
        val lo = Character.digit(hex[i * 2 + 1], 16)
        if (hi < 0 || lo < 0) return null
        out[i] = ((hi shl 4) or lo).toByte()
    }
    return out
}

/** `true` when every char of `s` is a lowercase hex digit. */
private fun isLowerHex(s: String): Boolean {
    for (c in s) {
        val isDigit = c in '0'..'9'
        val isLower = c in 'a'..'f'
        if (!isDigit && !isLower) return false
    }
    return true
}
