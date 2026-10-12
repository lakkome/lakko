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
 * payload (`{ v, d, g, p }`) and already through the wake gate
 * ([wakeAccountFor]). Only `g` + `p` (DS pointer) are load-bearing for the
 * preview; `d` has been spent on the gate, whose answer is [accountIdB64].
 */
public data class PushWake(
    /** 64 lowercase-hex chars — post-ADR-0042 the opaque 32-byte
     *  `push_gid` (reverse-resolved via [PushDecryptDeps.lookupRoute]);
     *  a pre-ADR-0042 legacy wake carries the raw 32-byte group_id.
     *  `null` for a non-group wake. */
    val groupHex: String?,
    /** DS message id of the message that triggered the wake, or `null`. */
    val pointer: Long?,
    /** The signed-in account the wake is bound to ([wakeAccountFor]). The
     *  ONLY account this wake is ever decrypted as (ADR-0042, amended
     *  2026-09-30): the reverse map and the legacy fallback are consulted
     *  for this account alone. */
    val accountIdB64: String,
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
public enum class ProcessedKind {
    CHAT,
    EDIT,
    DELETE,
    REACTION,
    TYPING,
    READ_RECEIPT,

    /** A call offer or end. Never a preview; it rings the phone instead
     *  (`docs/ux/calls.md` section 2.1). */
    CALL_SIGNALING,

    /** ADR-0061 section 6 — a chat envelope from a newer wire version. Its
     *  preview is the catalog sentence `chat.message.unsupported`; the core
     *  hands no text for it. */
    UNSUPPORTED,
}

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
    /** Set only for [ProcessedKind.CALL_SIGNALING]: the offer or end this
     *  row carried, as much of it as the ring needs. */
    val callSignal: me.lakko.app.calls.CallRingSignal? = null,
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

    /**
     * ADR-0053 client contract point 9 — the signer of this group's journal at
     * the snapshot's epoch, so the read-only fetch carries the proof a claimed
     * journal requires. Deriving it is an exporter call on the throwaway group,
     * not a write (C-59). `null` when it cannot be derived: the fetch then goes
     * unsigned, and a claimed journal's refusal falls back to the generic
     * notification.
     */
    public fun dsSigner(): me.lakko.app.ds.DsJournalSigner? = null
}

/**
 * READ-ONLY injected capabilities. Every method is a read or a pure
 * compute — there is deliberately NO write path (no `saveSnapshot` /
 * `advanceCursor`). The interface simply cannot persist, which is the
 * structural enforcement of the read-only-throwaway discipline.
 */
public interface PushDecryptDeps {
    /** ADR-0042: reverse-map an opaque wake `g` (the lowercase-hex
     *  `push_gid`) to the `group_id` hex [accountIdB64] filed it under
     *  ([PushRouteStore.lookup]), or `null` when this account has no such
     *  row. Another account's row is never an answer. A READ — never a
     *  write. */
    public fun lookupRoute(accountIdB64: String, pushGidHex: String): String?

    /** Legacy (pre-ADR-0042) fallback: does [accountIdB64] know `groupHex`
     *  as one of its own groups? Only the bound account's groups are
     *  searched; a group only another account knows is a stray wake. */
    public fun accountKnowsGroup(accountIdB64: String, groupHex: String): Boolean

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
     *  `afterId` (ascending id order), signed with [signer] (ADR-0053). Never
     *  claims or registers anything on a refusal: a background reader writes
     *  nothing (C-59). */
    public suspend fun dsFetch(
        groupIdBytes: ByteArray,
        afterId: Long,
        signer: me.lakko.app.ds.DsJournalSigner?,
    ): List<FetchedRow>

    /** The app's last-persisted per-group cursor (READ-ONLY), or
     *  `null`. Used as the fetch start so intervening commits replay
     *  and the epoch stays correct. */
    public fun startCursor(accountIdB64: String, groupHex: String): Long?

    /**
     * Is the conversation muted (`docs/ux/calls.md` section 2.1)?
     *
     * The same flag the chat list draws and the DeviceGroup journal syncs
     * between a person's devices, read from the state it is already persisted
     * in. A READ — never a write, like everything else on this interface.
     *
     * Defaulted to `false`, the RINGING answer: a wake whose account cannot be
     * resolved must lose no calls, and a store that fails to open must not
     * silence a phone.
     */
    public fun isMuted(accountIdB64: String, groupHex: String): Boolean = false

    /**
     * ADR-0065 R.5 — is the group the OLD segment of a one-to-one whose
     * conversation moved to a successor (the other person reset their account
     * keys and the reset was accepted)? Its only other senders are removed
     * devices: a wake for it fetches nothing, rings for nothing and shows
     * nothing. The successor's own wakes are ordinary.
     *
     * A READ of the continuity facts. Defaulted to `false`, and a store that
     * fails to open answers `false`: an unreadable flag must not silence a
     * live conversation.
     */
    public fun isClosedOneToOne(accountIdB64: String, groupHex: String): Boolean = false
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
): PushPreview? = decryptPushWake(
    wake,
    deps,
    nowMs = System.currentTimeMillis(),
    // A window of zero, not the real one. This wrapper keeps only the message
    // preview and throws the ring away, and a zero window makes every offer
    // read as already over — so the fold builds no ring at all, and this
    // function cannot start depending on a number it has no business reading.
    // (It also keeps it callable from the host JVM suite, which does not load
    // the native library the real window comes from.)
    ringWindowMs = 0L,
).preview

/**
 * Everything one wake means: a message preview, a call to ring for, and the
 * calls that have stopped.
 *
 */
public data class PushWakeOutcome(
    val preview: PushPreview?,
    /** An offer still inside its ring window, or `null`. */
    val ring: me.lakko.app.calls.IncomingCallRing?,
    /** Call ids whose `End` arrived in this batch — stop ringing for them. */
    val cancelledCallIds: List<String>,
    /** UX 16.4 — the wake named a closed one-to-one: post NOTHING for it, not
     *  even the generic banner [EMPTY] falls back to. */
    val suppressed: Boolean = false,
) {
    internal companion object {
        val EMPTY = PushWakeOutcome(preview = null, ring = null, cancelledCallIds = emptyList())
        val SUPPRESSED =
            PushWakeOutcome(preview = null, ring = null, cancelledCallIds = emptyList(), suppressed = true)
    }
}

/**
 * The full read-only background pass over one wake.
 *
 * [decryptPushPreview] is this function's message half, kept because the chat
 * preview is what most wakes are.
 *
 * @param nowMs the caller's clock, injected so the ring window is testable.
 * @param ringWindowMs the shared ring window
 *   (`crates/lakko-ui-rules/src/ring.rs`), injected rather than read from the
 *   core here: this function is host-tested and a JVM unit test does not load
 *   the native library. Production passes
 *   [uniffi.lakko_ffi_uniffi.callRingWindowMs].
 */
public suspend fun decryptPushWake(
    wake: PushWake?,
    deps: PushDecryptDeps,
    nowMs: Long,
    ringWindowMs: Long,
): PushWakeOutcome {
    // 1. Only GroupMessage wakes carry a 64-hex group + a pointer. A
    //    Wakeup / MailboxMessage wake is not handled in this slice.
    val rawGroupHex = wake?.groupHex ?: return PushWakeOutcome.EMPTY
    val pointer = wake.pointer ?: return PushWakeOutcome.EMPTY
    val groupHex = rawGroupHex.lowercase()
    if (groupHex.length != GROUP_HEX_LEN || !isLowerHex(groupHex)) return PushWakeOutcome.EMPTY

    // 2. Resolve the wake's `g` to the REAL DS group_id, for the bound
    //    account ONLY (ADR-0042, amended 2026-09-30 — the wake gate has
    //    already decided which account this wake is; nothing below may
    //    pick another). Post-ADR-0042 notify sends `g = hex(push_gid)`,
    //    an opaque routing id, so FIRST reverse-resolve it via the
    //    plaintext route index for `(bound account, push_gid)`; a hit
    //    yields the real group_id used for everything downstream
    //    (snapshot, loadGroup, DS fetch, cursor). A miss, a lookup error,
    //    or a malformed stored row FALLS BACK to the legacy raw-group_id
    //    path, which searches only the bound account's groups and fails
    //    closed to `null` (-> generic) on a stray wake.
    val accountIdB64 = wake.accountIdB64
    if (accountIdB64.isEmpty()) return PushWakeOutcome.EMPTY
    var account: ResolvedPushAccount? = null
    var realGroupHex = groupHex
    val routedHex = try {
        deps.lookupRoute(accountIdB64, groupHex)?.lowercase()
    } catch (_: Throwable) {
        null
    }
    if (routedHex != null) {
        val routedBytes =
            if (routedHex.length == GROUP_HEX_LEN && isLowerHex(routedHex)) {
                hexToBytes(routedHex)
            } else {
                null
            }
        if (routedBytes != null) {
            account = ResolvedPushAccount(
                accountIdB64 = accountIdB64,
                groupIdBytes = routedBytes,
            )
            realGroupHex = routedHex
        }
    }
    if (account == null) {
        // Legacy path: treat `g` as the raw group_id of one of the bound
        // account's own groups (a stray wake -> null).
        val knows = try {
            deps.accountKnowsGroup(accountIdB64, groupHex)
        } catch (_: Throwable) {
            false
        }
        if (!knows) return PushWakeOutcome.EMPTY
        val bytes = hexToBytes(groupHex) ?: return PushWakeOutcome.EMPTY
        account = ResolvedPushAccount(accountIdB64 = accountIdB64, groupIdBytes = bytes)
        realGroupHex = groupHex
    }

    // 2a. ADR-0065 R.5 — the old segment of a moved one-to-one. Decided
    //     BEFORE the seed is read or the journal fetched: nothing that arrives
    //     in it is shown, and a call offered from keys that are no longer the
    //     contact's never rings.
    val closedOneToOne = try {
        deps.isClosedOneToOne(account.accountIdB64, realGroupHex)
    } catch (_: Throwable) {
        false
    }
    if (closedOneToOne) return PushWakeOutcome.SUPPRESSED

    // 3. The AccountIdentity seed → the throwaway Identity.
    val seed = deps.loadIdentitySeed(account.accountIdB64) ?: return PushWakeOutcome.EMPTY

    // 4. The last-persisted MLS provider snapshot (the ratchet state
    //    the foreground app relies on — we only READ it).
    val snapshot = deps.loadSnapshot(account.accountIdB64) ?: return PushWakeOutcome.EMPTY

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
    } ?: return PushWakeOutcome.EMPTY

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
            deps.dsFetch(account.groupIdBytes, after, group.dsSigner())
        } catch (_: Throwable) {
            return PushWakeOutcome.EMPTY
        }

        // 7. Process each row through the TEMP group and keep the
        //    NEWEST frame that yields a user-visible preview. A frame
        //    that will not decrypt / authenticate (wrong epoch, our own
        //    echo, a control frame, a role-refused commit) is skipped —
        //    never guessed. NOTE: `process` mutates the THROWAWAY
        //    provider ONLY; it is never persisted.
        var latest: PushPreview? = null
        // The batch's call signaling, in arrival order. Folded once at the end
        // rather than acted on per row: a batch routinely carries both halves
        // of a finished call, and ringing for a call whose End is three rows
        // later leaves a phone the user has to silence by hand.
        val callSignals = ArrayList<me.lakko.app.calls.CallRingSignal>()
        for (row in rows) {
            val processed = try {
                group.process(row.payload)
            } catch (_: Throwable) {
                continue
            }
            processed?.callSignal?.let { callSignals += it }
            val body = previewBodyOf(processed) ?: continue
            // ADR-0048 UX §5 — the sender's resolved name when it is already
            // known on this device; the generic title otherwise.
            latest = PushPreview(title = processed?.senderName ?: "Lakko", body = body, tag = realGroupHex)
        }
        // `docs/ux/calls.md` section 2.1 — resolved only when this batch
        // actually rings for something, so an ordinary message wake never pays
        // for a store it does not need. What a muted offer DOES is decided
        // once for all three clients, in `crates/lakko-ui-rules/src/ring.rs`,
        // and applied in [me.lakko.app.calls.IncomingCallRinger]; this path
        // only supplies the fact.
        val muted = if (callSignals.isEmpty()) {
            false
        } else {
            try {
                deps.isMuted(account.accountIdB64, realGroupHex)
            } catch (_: Throwable) {
                false
            }
        }
        val calls = me.lakko.app.calls.foldCallRingSignals(
            groupIdHex = realGroupHex,
            signals = callSignals,
            nowMs = nowMs,
            windowMs = ringWindowMs,
            muted = muted,
        )
        return PushWakeOutcome(
            preview = latest,
            ring = calls.ring,
            cancelledCallIds = calls.cancelled,
        )
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
                    row.attachmentCount >= 1 -> me.lakko.app.l10n.Localization.strings.value.format(
                        "notification.message.attachments",
                        counts = mapOf("count" to row.attachmentCount.toULong()),
                    )
                    else -> null
                }
            }
        // ADR-0061 section 6 — a message this build cannot read: the
        // client's own sentence, never core text.
        ProcessedKind.UNSUPPORTED ->
            me.lakko.app.l10n.Localization.strings.value[me.lakko.app.ui.chat.UNSUPPORTED_MESSAGE_KEY]
        // An edit rewrites a prior message → surface the new text.
        ProcessedKind.EDIT ->
            if (row.text.isNotEmpty()) truncate(row.text) else null
        // delete / reaction / typing / read-receipt carry no standalone
        // preview — skip so an older real message stays the preview.
        ProcessedKind.DELETE,
        ProcessedKind.REACTION,
        ProcessedKind.TYPING,
        ProcessedKind.READ_RECEIPT,
        // A call is not a message preview. It leaves this function with
        // nothing and leaves `decryptPushWake` with a ring.
        ProcessedKind.CALL_SIGNALING,
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

/**
 * ADR-0065 R.5 — the conversation a woken group belongs to, for everything a
 * notification takes from the CONVERSATION rather than from the MLS group:
 * its mute, its grouping key and what a tap opens under. A successor group
 * resolves to the root of its lineage; any other group is its own
 * conversation. Pure: the registers are passed in (the background path reads
 * them from their plain mirror and writes nothing).
 */
public fun conversationHexForWake(groupHex: String, lineage: List<me.lakko.app.chat.LineageEntry>): String =
    try {
        me.lakko.app.chat.buildLineage(lineage).conversationOf(groupHex.lowercase())
    } catch (_: Throwable) {
        groupHex.lowercase()
    }

/**
 * The notification grouping key and stable id source of a wake.
 *
 * | the wake's `push_gid` resolves to a group | that group is a successor | key                      |
 * |-------------------------------------------|---------------------------|--------------------------|
 * | no                                        | -                         | the raw `push_gid`       |
 * | yes                                       | no                        | the raw `push_gid`       |
 * | yes                                       | yes                       | the conversation's root  |
 *
 * A conversation's first group keeps the key it always had; its successors
 * join that conversation's stack only once this device knows the lineage, and
 * a banner of the successor then replaces the conversation's banner instead
 * of standing beside it.
 */
public fun notificationGroupKey(
    rawPushGidHex: String?,
    resolvedGroupHex: String?,
    lineage: List<me.lakko.app.chat.LineageEntry>,
): String? {
    val raw = rawPushGidHex ?: return null
    val resolved = resolvedGroupHex?.lowercase()?.takeIf { it != raw.lowercase() } ?: return raw
    val conversation = conversationHexForWake(resolved, lineage)
    return if (conversation == resolved) raw else conversation
}
