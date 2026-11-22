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
 * payload (`{ v, s, g, p }`) and already through the wake gate
 * ([wakeAccountFor]). `g` + `p` (DS pointer) are load-bearing for the
 * preview, `s` for the route lookup (ADR-0078); the gate's answer is
 * [accountIdB64].
 */
public data class PushWake(
    /** 64 lowercase-hex chars — the opaque 32-byte `push_gid`
     *  (reverse-resolved via [PushDecryptDeps.lookupRoute]), or `null` for a
     *  non-group wake. */
    val groupHex: String?,
    /** DS message id of the message that triggered the wake, or `null`. */
    val pointer: Long?,
    /** The signed-in account the wake is bound to ([wakeAccountFor]). The
     *  ONLY account this wake is ever decrypted as (ADR-0042, amended
     *  2026-09-30): the reverse map is consulted for this account alone. */
    val accountIdB64: String,
    /** ADR-0078 — the push-gw subscription the wake was delivered for (`s`).
     *  The route lookup answers only when the account holds that id for the
     *  wake's `g`. */
    val subscriptionIdHex: String? = null,
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
    /** docs/ux/media-messages.md section 11 — what the attachments are, for
     *  the row's words (`3 photos`, `File`); empty when not read. */
    val previewAttachments: List<me.lakko.app.ui.chat.PreviewAttachment> = emptyList(),
    /** Section 11, ADR-0047 section 6 — the micro thumbnail of the first
     *  photo or video, carried inside the message, for the notification's
     *  picture; `null` when there is none. */
    val previewPicture: ByteArray? = null,
    /** ADR-0048 — the sender's name, when this device ALREADY knows one
     *  better than a fingerprint (read from the stored profiles; never
     *  fetched or verified here). `null` otherwise. */
    val senderName: String? = null,
    /** Set only for [ProcessedKind.CALL_SIGNALING]: the offer or end this
     *  row carried, as much of it as the ring needs. */
    val callSignal: me.lakko.app.calls.CallRingSignal? = null,
    /** The ACCOUNT key (standard base64) the MLS layer resolved the sender
     *  leaf to at receipt (C-68), or `null` when it did not resolve. What
     *  the superseded-sender rule and the re-send marker read. */
    val senderAccountB64: String? = null,
    /** ADR-0073 R.7 case 1 — the re-send marker a chat body carried. */
    val resendOrigin: me.lakko.app.chat.ResendOrigin? = null,
    /** Reactions only: add (`true`) or take off. */
    val reactionAdd: Boolean? = null,
    /** Reactions only: the emoji. */
    val reactionEmoji: String? = null,
    /** Reactions only: the account key (base64) of the reacted-to message's
     *  author, as the reactor named it. */
    val reactionTargetAuthorB64: String? = null,
    /** Edits and deletes only: the DS id of the message the op names. */
    val target: Long? = null,
)

/** What the superseded-sender rule reads off the group as the batch left it
 *  ([me.lakko.app.chat.unrepairableBy]). All reads. */
public data class WakeGroupFacts(
    val kind: me.lakko.app.chat.ConversationKind,
    val rosterPersonKeysB64: List<String>,
    /** `null` = no role table. */
    val ownerKeyB64: String?,
    val ownPersonKeyB64: String?,
)

/** The notification preview the FMS renders when the decrypt succeeds. */
public data class PushPreview(
    val title: String,
    val body: String,
    /** Notification tag (the group hex — collapses a burst into one
     *  OS notification). */
    val tag: String,
    /** docs/ux/media-messages.md section 11, ADR-0047 section 6 — the micro
     *  thumbnail of the first photo or video (JPEG, at most 4096 bytes),
     *  decoded in memory for the notification's picture. Set only on a
     *  preview that shows the message's text; `null` otherwise. */
    val picture: ByteArray? = null,
    /** `docs/ux/message-actions.md` section 9 — the DS id of the message this
     *  preview shows (in the real group [tag] names), carried into the
     *  notification's extras so a later deletion of it withdraws the
     *  notification. `null` for a preview that shows no message text (the
     *  generic and unnamed-sender banners, a reaction line). */
    val messageId: Long? = null,
    /** The account key (base64) of that message's author, when known. */
    val authorKeyB64: String? = null,
)

/**
 * Section 11 — the bytes the notification draws as its picture, or `null`.
 * The picture is shown exactly when the message's text is: only a preview
 * that carries real text has one (the generic "New message" and the
 * unnamed-sender fallbacks never do), and the bytes are checked against the
 * bound and the JPEG signature again here.
 */
public fun notificationPictureBytes(preview: PushPreview?): ByteArray? {
    if (preview == null || preview.body.isBlank()) return null
    return me.lakko.app.media.inlineThumbOrNull(preview.picture)
}

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

    /**
     * ADR-0073 R.5 / R.8 — the roster, the kind and the owner of the
     * throwaway group as the rows processed so far left it. Reads of the
     * in-memory group; nothing is written (C-59). `null` when they cannot be
     * read: the rule then drops only what needs no such fact.
     */
    public fun facts(): WakeGroupFacts? = null
}

/**
 * READ-ONLY injected capabilities. Every method is a read or a pure
 * compute — there is deliberately NO write path (no `saveSnapshot` /
 * `advanceCursor`). The interface simply cannot persist, which is the
 * structural enforcement of the read-only-throwaway discipline.
 */
public interface PushDecryptDeps {
    /** ADR-0042 with ADR-0078: reverse-map an opaque wake `g` (the
     *  lowercase-hex `push_gid`) to the `group_id` hex [accountIdB64] filed
     *  it under ([PushRouteStore.lookup]), but only when this device's
     *  subscription id for that row is [subscriptionIdHex] (the wake's `s`);
     *  `null` otherwise. Another account's row is never an answer. A READ —
     *  never a write. */
    public fun lookupRoute(accountIdB64: String, pushGidHex: String, subscriptionIdHex: String?): String?

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

    /** reactions-and-emoji section 12 — this device's `Reactions`
     *  notification switch (on by default). A READ. */
    public fun reactionNotificationsOn(accountIdB64: String): Boolean = true

    /** The group's shared name as this device stores it, or `null`. A READ. */
    public fun groupName(accountIdB64: String, groupHex: String): String? = null

    /**
     * ADR-0073 R.5 — is the group the OLD segment of a one-to-one whose
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

    /**
     * ADR-0073 R.5 / R.8 — the account keys accepted resets RETIRED, from
     * the reset ledger the identity gate writes. A READ. Defaulted to empty,
     * and a store that fails to open answers empty: nothing is dropped on an
     * unreadable ledger.
     */
    public fun supersededKeys(accountIdB64: String): Set<String> = emptySet()

    /** ADR-0073 R.5 — whether the conversation [groupHex] was a segment of
     *  has moved to a successor (the continuity facts). A READ. */
    public fun conversationMoved(accountIdB64: String, groupHex: String): Boolean = false

    /** ADR-0073 R.5 — the lineage registers, from their plain mirror. A READ. */
    public fun lineage(accountIdB64: String): List<me.lakko.app.chat.LineageEntry> = emptyList()

    /**
     * ADR-0073 R.7 case 1 — the row `(group, message id)` of the app's own
     * stored message log, as the re-send marker needs it, or `null` when the
     * app holds no such row. A READ of the log the start cursor is already
     * read from; this path never writes it.
     */
    public fun heldRow(accountIdB64: String, groupHex: String, messageId: Long): me.lakko.app.chat.HeldRow? = null

    /**
     * The row `(group, message id)` of the app's own stored message log, read
     * FRESH on every call (never cached: the running app may write it while
     * this read is under way), or `null` when the log holds no such row. A
     * READ; this path never writes the log.
     */
    public fun storedMessage(accountIdB64: String, groupHex: String, messageId: Long): StoredWakeMessage? = null

    /** The name this device stores for the person [personKeyB64] (an account
     *  key), or `null`. A READ. */
    public fun personName(accountIdB64: String, personKeyB64: String): String? = null

    /**
     * ADR-0099 section 6 — whether [senderAccountB64] is blocked by the
     * account, read from the persisted synced state (a READ, C-59).
     */
    public fun isBlocked(accountIdB64: String, senderAccountB64: String?): Boolean = false
}

/**
 * ADR-0073 R.5 / R.8 and R.7 case 1 — whether a processed row of a wake is
 * NOT acted on: nothing is shown for it and it rings for nothing.
 *
 * | row                                                              | answer |
 * |------------------------------------------------------------------|--------|
 * | a chat row, an edit, ... from a key an accepted reset retired    | yes    |
 * | a call offer from such a key, the conversation moved             | yes    |
 * | a call offer from such a key, unrepairable because of its reset  | yes    |
 * | a call offer from such a key, the group is repaired in place     | no     |
 * | a re-sent message whose original this device holds               | yes    |
 * | anything else                                                    | no     |
 *
 * The first four rows are [me.lakko.app.chat.supersededActDropped], the same
 * function the foreground poll applies; the fifth is
 * [me.lakko.app.chat.storedResendVerdict] over the stored message log.
 */
internal fun wakeRowSuppressed(
    row: ProcessedRow,
    groupHex: String,
    supersededKeys: Set<String>,
    facts: () -> WakeGroupFacts?,
    moved: () -> Boolean,
    lineage: () -> me.lakko.app.chat.Lineage,
    heldRowOf: (groupHex: String, messageId: Long) -> me.lakko.app.chat.HeldRow?,
): Boolean {
    if (me.lakko.app.chat.dropsSupersededSender(row.senderAccountB64, supersededKeys)) {
        val act = if (row.kind == ProcessedKind.CALL_SIGNALING) me.lakko.app.chat.SenderAct.CALL else me.lakko.app.chat.SenderAct.ROW
        val dropped = act == me.lakko.app.chat.SenderAct.ROW || run {
            val f = facts()
            me.lakko.app.chat.supersededActDropped(
                act = act,
                senderKeyB64 = row.senderAccountB64,
                supersededKeys = supersededKeys,
                unrepairableKeyB64 = f?.let {
                    me.lakko.app.chat.unrepairableBy(
                        kind = it.kind,
                        rosterPersonKeysB64 = it.rosterPersonKeysB64,
                        ownPersonKeyB64 = it.ownPersonKeyB64,
                        ownerKeyB64 = it.ownerKeyB64,
                        supersededKeys = supersededKeys,
                    )
                },
                moved = moved(),
            )
        }
        if (dropped) return true
    }
    val marker = row.resendOrigin ?: return false
    if (row.kind != ProcessedKind.CHAT) return false
    return me.lakko.app.chat.storedResendVerdict(
        marker = marker,
        receivedGroupHex = groupHex,
        lineage = lineage(),
        heldRowOf = heldRowOf,
        ownPersonKeyB64 = facts()?.ownPersonKeyB64,
        senderKeyB64 = row.senderAccountB64,
    ) == me.lakko.app.chat.ResendVerdict.SKIP
}

/**
 * ADR-0099 section 6 — whether a row of a blocked sender posts nothing: a
 * call offer is declined without a ring ([me.lakko.app.moderation.declinesCallSilently]),
 * anything else notifies for nothing ([me.lakko.app.moderation.suppressesNotification]).
 * Called only for a sender the register holds.
 */
internal fun blockedRowSilenced(row: ProcessedRow): Boolean =
    if (row.kind == ProcessedKind.CALL_SIGNALING) {
        me.lakko.app.moderation.declinesCallSilently(row.senderAccountB64) { true }
    } else {
        me.lakko.app.moderation.suppressesNotification(row.senderAccountB64) { true }
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
    /** The read ran to the end: the snapshot was restored and the rows were
     *  fetched and processed. `false` for every early return, where nothing
     *  is known about the wake and the generic banner stands. */
    val readRan: Boolean = false,
    /** Rows that did not decrypt and that the app's message log does not
     *  hold, i.e. a message this device cannot read yet. A row that does not
     *  decrypt but IS held is this device's own message coming back (a
     *  sender cannot open its own message) and is not counted. */
    val unreadableRows: Int = 0,
    /**
     * A preview built from the app's own stored message log, for a message
     * from someone else that the running app had already applied before this
     * background read (the app's poll and this read share one journal, and a
     * message the app applied cannot be decrypted a second time). Shown only
     * when the app is not on screen: on screen the app shows the message
     * itself. `null` when no such row exists.
     */
    val storedPreview: PushPreview? = null,
    /** Why this wake would show the generic "New message" banner, for one
     *  plain log line (no identifiers). `null` when it has a real preview, a
     *  ring, or posts nothing. */
    val fallbackReason: WakeFallbackReason? = null,
    /** A short detail for [fallbackReason] (an HTTP status, an exception
     *  class), never an identifier ([fetchFailureDetail]). */
    val fallbackDetail: String? = null,
    /** `docs/ux/message-actions.md` section 9 — the real DS group (hex) the
     *  wake was read for, `null` when the read did not get that far. */
    val groupHex: String? = null,
    /** The deletions this wake carried, each with its sender's account key
     *  (unchecked against the author here): a notification already on screen
     *  for one of them is withdrawn ([withdrawDeletedMessageNotifications]). */
    val deletes: List<AppliedDelete> = emptyList(),
) {
    /**
     * Whether a wake that showed no preview and rang for nothing still posts
     * the generic "New message" banner.
     *
     * A banner means a message from someone else. A wake that brought only
     * this device's own message, a message from another device of the same
     * account, or service traffic (receipts, typing, reactions, profiles, ...)
     * posts nothing. The generic banner is kept for what cannot be told
     * apart: a read that did not run, or a row that did not decrypt and is
     * not ours, and only while the app is not on screen (on screen the app
     * shows the conversation itself). A message the app's stored log already
     * holds ([storedPreview]) is never the generic banner: off screen it is
     * shown with its text, on screen the app shows it.
     */
    public fun genericBannerWanted(appOnScreen: Boolean): Boolean =
        preview == null && ring == null && storedPreview == null &&
            (!readRan || (!appOnScreen && unreadableRows > 0))


    /**
     * The preview the notification shows: the decrypted one, else the stored
     * one when the app is not on screen.
     */
    public fun shownPreview(appOnScreen: Boolean): PushPreview? =
        preview ?: storedPreview?.takeIf { !appOnScreen }

    internal companion object {
        val EMPTY = PushWakeOutcome(preview = null, ring = null, cancelledCallIds = emptyList())

        /** [EMPTY] with the reason it is empty. */
        fun emptyBecause(reason: WakeFallbackReason, storedPreview: PushPreview? = null): PushWakeOutcome =
            EMPTY.copy(fallbackReason = reason, storedPreview = storedPreview)
        val SUPPRESSED =
            PushWakeOutcome(preview = null, ring = null, cancelledCallIds = emptyList(), suppressed = true)
    }
}

/**
 * Why a wake falls back to the generic "New message" banner. Each value is
 * logged as one plain line with no identifier, so a generic banner is never
 * silent: a defect report can say which step failed.
 */
public enum class WakeFallbackReason(public val logText: String) {
    /** The wake named no group or no message pointer. */
    NO_POINTER("the wake carries no message pointer"),

    /** The wake's group field is not a 64-hex id. */
    BAD_GROUP_ID("the wake's group field is malformed"),

    /** The wake is bound to no account. */
    NO_ACCOUNT("the wake is bound to no account"),

    /** The local route index holds no group for the wake's routing id. */
    NO_ROUTE("no local route for the wake's routing id"),

    /** No device identity seed is stored for the account. */
    NO_SEED("no device identity seed stored"),

    /** No usable MLS snapshot (absent, unreadable, or taken under another
     *  identity). */
    NO_SNAPSHOT("no usable MLS snapshot"),

    /** The snapshot holds no such group, or it would not open. */
    GROUP_NOT_OPENED("the group did not open from the snapshot"),

    /** The Delivery Service read failed. */
    FETCH_FAILED("the journal read failed"),

    /** A row did not decrypt and the app's log does not hold it. */
    UNREADABLE_ROWS("a row did not decrypt and the app does not hold it"),

    /** A message decrypted but its sender has no name on this device. */
    UNNAMED_SENDER("the sender has no name on this device"),

    /** The background read ran out of its time budget. */
    TIMEOUT("the background read timed out"),

    /** The background read threw. */
    READ_FAILED("the background read failed"),

    /** The read ran and found nothing it could explain. */
    NOTHING_FOUND("the read found no message to show"),
}

/**
 * The pause before a background journal read is tried a second time after a
 * network failure. Chosen, not measured; with the stored-log retry it stays
 * well inside the FCM budget.
 */
internal const val FETCH_RETRY_DELAY_MS: Long = 1_000L

/** Whether a failed journal read is worth one more try: a network failure,
 *  never an HTTP answer. */
internal fun isTransientFetchFailure(cause: Throwable): Boolean =
    cause is me.lakko.app.ds.DsNetworkError || cause is java.io.IOException

/**
 * What a failed journal read looked like, for the log: the HTTP status and,
 * for a refused proof, whether the device clock was off; else the class of
 * the network failure or of the exception. Never a message text, which can
 * carry an endpoint.
 */
public fun fetchFailureDetail(cause: Throwable): String = when (cause) {
    is me.lakko.app.ds.DsProofRefusedError ->
        if (cause.clockSkewed) "http 401, proof refused, device clock off" else "http 401, proof refused"
    is me.lakko.app.ds.DsHttpError -> "http ${cause.status}"
    is me.lakko.app.ds.DsNetworkError -> "network ${cause.cause?.javaClass?.simpleName ?: "unknown"}"
    else -> cause.javaClass.simpleName.ifEmpty { "unknown" }
}

/**
 * A message row of the app's own stored message log, as a background preview
 * needs it ([PushDecryptDeps.storedMessage]).
 */
public data class StoredWakeMessage(
    /** `true` for a message from someone else; own-account rows (this device
     *  or another device of the account) and system rows are `false`. */
    val incoming: Boolean,
    /** The author's account key (standard base64) as resolved at receipt. */
    val authorKeyB64: String?,
    val text: String,
    val attachments: List<me.lakko.app.ui.chat.PreviewAttachment>,
    val deleted: Boolean,
)

/**
 * How long a background read waits before reading the app's message log a
 * second time, when a row did not decrypt and the log did not hold it yet:
 * the running app writes its MLS snapshot just before its message log, so a
 * read that falls between the two finds the row in neither. Chosen, not
 * measured; well inside the FCM budget.
 */
internal const val STORED_LOG_RETRY_MS: Long = 1_500L

/**
 * | a preview | a ring | rows not acted on | the wake posts nothing |
 * |-----------|--------|-------------------|------------------------|
 * | yes       | any    | any               | no                     |
 * | no        | yes    | any               | no                     |
 * | no        | no     | none              | no (generic fallback)  |
 * | no        | no     | one or more       | yes                    |
 */
internal fun wakePostsNothing(hasPreview: Boolean, rings: Boolean, suppressedRows: Int): Boolean =
    !hasPreview && !rings && suppressedRows > 0

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
    val rawGroupHex = wake?.groupHex ?: return PushWakeOutcome.emptyBecause(WakeFallbackReason.NO_POINTER)
    val pointer = wake.pointer ?: return PushWakeOutcome.emptyBecause(WakeFallbackReason.NO_POINTER)
    val groupHex = rawGroupHex.lowercase()
    if (groupHex.length != GROUP_HEX_LEN || !isLowerHex(groupHex)) {
        return PushWakeOutcome.emptyBecause(WakeFallbackReason.BAD_GROUP_ID)
    }

    // 2. Resolve the wake's `g` to the REAL DS group_id, for the bound
    //    account ONLY (ADR-0042, amended 2026-09-30 — the wake gate has
    //    already decided which account this wake is; nothing below may
    //    pick another). Post-ADR-0042 notify sends `g = hex(push_gid)`,
    //    an opaque routing id, so FIRST reverse-resolve it via the
    //    plaintext route index for `(bound account, push_gid)`; a hit
    //    yields the real group_id used for everything downstream
    //    (snapshot, loadGroup, DS fetch, cursor). ADR-0078: only when the
    //    row's subscription id is the wake's `s`. A miss, a lookup error, or
    //    a malformed stored row is the generic notification (the raw
    //    group_id fallback of the pre-ADR-0042 wake is gone).
    val accountIdB64 = wake.accountIdB64
    if (accountIdB64.isEmpty()) return PushWakeOutcome.emptyBecause(WakeFallbackReason.NO_ACCOUNT)
    val routedHex = try {
        deps.lookupRoute(accountIdB64, groupHex, wake.subscriptionIdHex)?.lowercase()
    } catch (_: Throwable) {
        null
    } ?: return PushWakeOutcome.emptyBecause(WakeFallbackReason.NO_ROUTE)
    if (routedHex.length != GROUP_HEX_LEN || !isLowerHex(routedHex)) {
        return PushWakeOutcome.emptyBecause(WakeFallbackReason.NO_ROUTE)
    }
    val routedBytes = hexToBytes(routedHex) ?: return PushWakeOutcome.emptyBecause(WakeFallbackReason.NO_ROUTE)
    val account = ResolvedPushAccount(accountIdB64 = accountIdB64, groupIdBytes = routedBytes)
    val realGroupHex = routedHex

    // 2a. ADR-0073 R.5 — the old segment of a moved one-to-one. Decided
    //     BEFORE the seed is read or the journal fetched: nothing that arrives
    //     in it is shown, and a call offered from keys that are no longer the
    //     contact's never rings.
    val closedOneToOne = try {
        deps.isClosedOneToOne(account.accountIdB64, realGroupHex)
    } catch (_: Throwable) {
        false
    }
    if (closedOneToOne) return PushWakeOutcome.SUPPRESSED

    // From here on the group is known. A read that cannot run past this
    // point still looks in the app's own stored log for the wake's message:
    // the running app may have applied it already, and its snapshot is then
    // past what a second decrypt could open, or mid-write.
    val stored = StoredFallback(deps, account.accountIdB64, realGroupHex)
    suspend fun emptyWithStored(reason: WakeFallbackReason): PushWakeOutcome {
        val fromLog = stored.preview(listOf(pointer), kind = null)
        val shownReason = if (fromLog != null && stored.unnamed) WakeFallbackReason.UNNAMED_SENDER else reason
        return PushWakeOutcome.emptyBecause(shownReason, fromLog)
    }

    // 3. The AccountIdentity seed → the throwaway Identity.
    val seed = deps.loadIdentitySeed(account.accountIdB64) ?: return emptyWithStored(WakeFallbackReason.NO_SEED)

    // 4. The last-persisted MLS provider snapshot (the ratchet state
    //    the foreground app relies on — we only READ it).
    val snapshot = deps.loadSnapshot(account.accountIdB64) ?: run {
        seed.fill(0)
        return emptyWithStored(WakeFallbackReason.NO_SNAPSHOT)
    }

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
    } ?: return emptyWithStored(WakeFallbackReason.GROUP_NOT_OPENED)

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

        // A network failure is read once more after a short pause: a wake
        // that starts a backgrounded process can find the app's network
        // still being restored (seen on a Pixel 2026-11-22: one read failed
        // right after its connection opened, the next wake's read succeeded
        // and the DS refused nothing). A refusal (4xx) is not retried.
        val rows = try {
            try {
                deps.dsFetch(account.groupIdBytes, after, group.dsSigner())
            } catch (first: Throwable) {
                if (first is kotlinx.coroutines.CancellationException || !isTransientFetchFailure(first)) throw first
                kotlinx.coroutines.delay(FETCH_RETRY_DELAY_MS)
                deps.dsFetch(account.groupIdBytes, after, group.dsSigner())
            }
        } catch (cancel: kotlinx.coroutines.CancellationException) {
            throw cancel
        } catch (cause: Throwable) {
            return emptyWithStored(WakeFallbackReason.FETCH_FAILED).copy(fallbackDetail = fetchFailureDetail(cause))
        }

        // 7. Process each row through the TEMP group and keep the
        //    NEWEST frame that yields a user-visible preview. A frame
        //    that will not decrypt / authenticate (wrong epoch, our own
        //    echo, a control frame, a role-refused commit) is skipped —
        //    never guessed. NOTE: `process` mutates the THROWAWAY
        //    provider ONLY; it is never persisted.
        var latest: PushPreview? = null
        var suppressedRows = 0
        // The batch's call signaling, in arrival order. Folded once at the end
        // rather than acted on per row: a batch routinely carries both halves
        // of a finished call, and ringing for a call whose End is three rows
        // later leaves a phone the user has to silence by hand.
        val callSignals = ArrayList<me.lakko.app.calls.CallRingSignal>()
        // ADR-0073 — what the two silent rules read, each store opened at
        // most once per wake and only when a row asks. All READS (C-59); a
        // store that does not open drops nothing.
        val superseded = try {
            deps.supersededKeys(account.accountIdB64)
        } catch (_: Throwable) {
            emptySet()
        }
        val moved by lazy {
            try {
                deps.conversationMoved(account.accountIdB64, realGroupHex)
            } catch (_: Throwable) {
                false
            }
        }
        val lineage by lazy {
            try {
                me.lakko.app.chat.buildLineage(deps.lineage(account.accountIdB64))
            } catch (_: Throwable) {
                me.lakko.app.chat.NO_LINEAGE
            }
        }
        // The account key of this device's person, read once and only when a
        // decrypted message asks: a message from it (another device of the
        // same account) is not a notification.
        val ownPersonKeyB64 by lazy { runCatching { group.facts()?.ownPersonKeyB64 }.getOrNull() }
        // The notification's shape: the conversation's name as the title, in
        // a group the sender before the text. Read once, when a row asks.
        val conversationKind by lazy { runCatching { group.facts()?.kind }.getOrNull() }
        val groupName by lazy { runCatching { deps.groupName(account.accountIdB64, realGroupHex) }.getOrNull() }
        var unreadableRows = 0
        // The wake's own rows (the pointer and after) that did not decrypt and
        // are not this device's own message: the running app may have applied
        // them first, in which case its stored log holds the text.
        val failedWakeRows = ArrayList<Long>()
        var latestUnnamed = false
        // `docs/ux/message-actions.md` section 9 — every preview this batch
        // produced, keyed on the message it describes, newest last: a delete
        // in the same batch takes its message's preview away, and the
        // notification describes the newest message that is not deleted.
        val previews = LinkedHashMap<Long, Pair<PushPreview, Boolean>>()
        val deletes = ArrayList<AppliedDelete>()
        fun putPreview(messageId: Long, preview: PushPreview, unnamed: Boolean) {
            previews.remove(messageId)
            previews[messageId] = preview to unnamed
            latest = preview
            latestUnnamed = unnamed
        }
        for (row in rows) {
            val processed = try {
                group.process(row.payload)
            } catch (cause: Throwable) {
                // A sender cannot open its own message. The app's log holds a
                // row it sent (or one it already received), so a held id is
                // quiet; anything else is a message this device cannot read.
                if (isOwnMessageEcho(cause)) continue
                if (row.id >= pointer) failedWakeRows += row.id
                val held = try {
                    deps.heldRow(account.accountIdB64, realGroupHex, row.id) != null
                } catch (_: Throwable) {
                    false
                }
                if (!held) unreadableRows += 1
                continue
            }
            if (processed != null) {
                val suppressed = try {
                    wakeRowSuppressed(
                        row = processed,
                        groupHex = realGroupHex,
                        supersededKeys = superseded,
                        facts = { runCatching { group.facts() }.getOrNull() },
                        moved = { moved },
                        lineage = { lineage },
                        heldRowOf = { hex, id -> deps.heldRow(account.accountIdB64, hex, id) },
                    )
                } catch (_: Throwable) {
                    false
                }
                // Neither a preview nor a ring: an older real message stays
                // the preview, and a wake that carried nothing else posts
                // nothing at all.
                if (suppressed) {
                    suppressedRows += 1
                    continue
                }
                // ADR-0099 section 6 — nothing of a blocked person notifies,
                // and their calls do not ring (declined without a ring).
                val blocked = runCatching { deps.isBlocked(account.accountIdB64, processed.senderAccountB64) }
                    .getOrDefault(false)
                if (blocked && blockedRowSilenced(processed)) {
                    suppressedRows += 1
                    continue
                }
            }
            processed?.callSignal?.let { callSignals += it }
            val fromOwnAccount = processed?.senderAccountB64 != null &&
                processed.callSignal == null &&
                processed.senderAccountB64 == ownPersonKeyB64
            if (fromOwnAccount) continue
            // Section 9 — a deletion never makes a notification; it takes away
            // the preview of the message it names when this batch made one.
            if (processed?.kind == ProcessedKind.DELETE) {
                suppressedRows += 1
                val target = processed.target
                val deleter = processed.senderAccountB64
                // Only the message's author deletes it (ADR-0023). This read
                // holds no message log, so the check is against the author the
                // preview recorded; a delete from anyone else takes nothing.
                if (target != null && deleter != null) {
                    deletes += AppliedDelete(target, deleter)
                    val held = previews[target]
                    if (held != null && (held.first.authorKeyB64 == null || held.first.authorKeyB64 == deleter)) {
                        previews.remove(target)
                        val newest = previews.values.lastOrNull()
                        latest = newest?.first
                        latestUnnamed = newest?.second ?: false
                    }
                }
                continue
            }
            // reactions-and-emoji section 12 — a reaction added to one of
            // this person's own messages is a notification of its own,
            // "Anna reacted ❤️ to your message", unless the switch is off or
            // the conversation is muted. No excerpt: this path holds no
            // message log.
            val reactionBody = processed?.let { p ->
                reactionNotificationBody(
                    row = p,
                    ownPersonKeyB64 = ownPersonKeyB64,
                    enabled = { runCatching { deps.reactionNotificationsOn(account.accountIdB64) }.getOrDefault(true) },
                    muted = { runCatching { deps.isMuted(account.accountIdB64, realGroupHex) }.getOrDefault(false) },
                )
            }
            if (reactionBody != null) {
                // The reaction line names the reactor itself: no prefix.
                val shape = notificationShape(conversationKind, groupName, processed?.senderName, reactionBody, prefixSender = false)
                putPreview(
                    row.id,
                    shape?.let { PushPreview(title = it.first, body = it.second, tag = realGroupHex) }
                        ?: unnamedSenderPreview(realGroupHex),
                    unnamed = shape == null,
                )
                continue
            }
            val body = previewBodyOf(processed) ?: continue
            // The conversation's name as the title; in a group the sender's
            // name before the text.
            val shape = notificationShape(conversationKind, groupName, processed?.senderName, body)
            // ADR-0047 section 6 — the picture rides only a preview that shows
            // the text; the unnamed-sender fallback shows neither. An edit's
            // preview describes the message it names.
            val describes = if (processed?.kind == ProcessedKind.EDIT) processed.target ?: row.id else row.id
            putPreview(
                describes,
                shape?.let {
                    PushPreview(
                        title = it.first,
                        body = it.second,
                        tag = realGroupHex,
                        picture = me.lakko.app.media.inlineThumbOrNull(processed?.previewPicture),
                        messageId = describes,
                        authorKeyB64 = processed?.senderAccountB64,
                    )
                } ?: unnamedSenderPreview(realGroupHex),
                unnamed = shape == null,
            )
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
        // Nothing decrypted to a preview, but rows of this wake did not
        // decrypt: read them from the app's own log (the app applied them).
        val storedPreview = if (latest == null && calls.ring == null && failedWakeRows.isNotEmpty()) {
            stored.preview(failedWakeRows, kind = conversationKind)
        } else {
            null
        }
        val postsNothing = wakePostsNothing(latest != null, calls.ring != null, suppressedRows) && storedPreview == null
        val fallbackReason = when {
            postsNothing || calls.ring != null -> null
            latest != null -> if (latestUnnamed) WakeFallbackReason.UNNAMED_SENDER else null
            storedPreview != null -> if (stored.unnamed) WakeFallbackReason.UNNAMED_SENDER else null
            unreadableRows > 0 -> WakeFallbackReason.UNREADABLE_ROWS
            else -> WakeFallbackReason.NOTHING_FOUND
        }
        return PushWakeOutcome(
            preview = latest,
            ring = calls.ring,
            cancelledCallIds = calls.cancelled,
            // ADR-0073 — the wake carried only what is not acted on (a row
            // of a retired key, or a re-send this device holds the original
            // of): nothing is posted for it, not even the generic banner.
            suppressed = postsNothing,
            readRan = true,
            // A row the stored log explained is not unreadable any more.
            unreadableRows = if (storedPreview != null) 0 else unreadableRows,
            storedPreview = storedPreview,
            fallbackReason = fallbackReason,
            groupHex = realGroupHex,
            deletes = deletes,
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
 * The preview of a wake's message from the app's own stored message log
 * ([PushDecryptDeps.storedMessage]), for a message from someone else that the
 * running app applied before the background read got to it.
 *
 * Why it is needed: the app's open chat (or its receive sweep) and the
 * background read fetch the same journal rows. A message the app applied has
 * its key consumed (forward secrecy), so the background decrypt of it fails;
 * before this, such a row read as "this device's own message" and the wake
 * posted nothing, or, while the app was still writing, the generic banner.
 *
 * Only an INCOMING row is used (the log stores this device's and the
 * account's other devices' messages as outgoing), never a deleted one, never
 * one from a blocked sender or a key a reset retired. The newest such row
 * among [ids] wins. When a row is in the log under none of these ids, the log
 * is read once more after [STORED_LOG_RETRY_MS]: the app writes its snapshot
 * just before its log. All reads (C-59).
 */
internal class StoredFallback(
    private val deps: PushDecryptDeps,
    private val accountIdB64: String,
    private val groupHex: String,
    private val retryDelayMs: Long = STORED_LOG_RETRY_MS,
) {
    /** `true` when the last [preview] found a message but no name for its
     *  sender (the generic banner, logged as such). */
    var unnamed: Boolean = false
        private set

    suspend fun preview(ids: List<Long>, kind: me.lakko.app.chat.ConversationKind?): PushPreview? {
        val wanted = ids.distinct().sortedDescending()
        if (wanted.isEmpty()) return null
        val first = scan(wanted, kind)
        if (first.preview != null || !first.missing) return first.preview
        kotlinx.coroutines.delay(retryDelayMs)
        return scan(wanted, kind).preview
    }

    private class Scan(val preview: PushPreview?, val missing: Boolean)

    private fun scan(ids: List<Long>, kind: me.lakko.app.chat.ConversationKind?): Scan {
        var missing = false
        for (id in ids) {
            val row = try {
                deps.storedMessage(accountIdB64, groupHex, id)
            } catch (_: Throwable) {
                null
            }
            if (row == null) {
                missing = true
                continue
            }
            if (!row.incoming || row.deleted) continue
            val author = row.authorKeyB64?.takeIf { it.isNotEmpty() }
            val blocked = runCatching { deps.isBlocked(accountIdB64, author) }.getOrDefault(false)
            if (blocked) continue
            val superseded = runCatching { deps.supersededKeys(accountIdB64) }.getOrDefault(emptySet())
            if (me.lakko.app.chat.dropsSupersededSender(author, superseded)) continue
            val body = storedPreviewBody(row) ?: continue
            val name = author?.let { runCatching { deps.personName(accountIdB64, it) }.getOrNull() }
            val groupName = runCatching { deps.groupName(accountIdB64, groupHex) }.getOrNull()
            val shape = notificationShape(kind, groupName, name, body)
            unnamed = shape == null
            val preview = shape?.let {
                PushPreview(title = it.first, body = it.second, tag = groupHex, messageId = id, authorKeyB64 = author)
            } ?: unnamedSenderPreview(groupHex)
            return Scan(preview, missing)
        }
        return Scan(null, missing)
    }
}

/**
 * The row `(group, message id)` of a stored message log as a background
 * preview reads it, or `null` when the log holds no such row. Pure.
 */
internal fun storedWakeMessageIn(
    log: me.lakko.app.chat.MessageLogPayload?,
    groupHex: String,
    messageId: Long,
): StoredWakeMessage? {
    if (log == null || messageId <= 0) return null
    val key = groupHex.lowercase()
    val group = log.groups[key] ?: log.groups.entries.firstOrNull { it.key.lowercase() == key }?.value ?: return null
    val row = group.messages.firstOrNull { it.messageId == messageId } ?: return null
    return StoredWakeMessage(
        incoming = me.lakko.app.chat.heldRowDirectionOf(row.direction) == me.lakko.app.chat.HeldRowDirection.INCOMING,
        authorKeyB64 = row.authorKeyB64,
        text = row.text,
        attachments = row.attachments.map { me.lakko.app.ui.chat.PreviewAttachment(it.kind, it.mime) },
        deleted = row.deleted,
    )
}

/** The body line of a stored message, the same words a decrypted one gets. */
private fun storedPreviewBody(row: StoredWakeMessage): String? =
    if (row.attachments.isNotEmpty()) {
        me.lakko.app.ui.chat.messagePreviewBody(row.text, row.attachments)
            .takeIf { it.isNotEmpty() }?.let(::truncate)
    } else {
        row.text.trim().takeIf { it.isNotEmpty() }?.let(::truncate)
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
            if (row.previewAttachments.isNotEmpty()) {
                // Section 11 — the same words and text as the list row.
                me.lakko.app.ui.chat.messagePreviewBody(row.text, row.previewAttachments)
                    .takeIf { it.isNotEmpty() }?.let(::truncate)
            } else if (row.text.isNotEmpty()) {
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

/**
 * reactions-and-emoji section 12 — the body of a reaction notification, or
 * `null`: only an ADD naming this person's account as the message's author,
 * from someone else, with the switch on and the conversation not muted. Pure
 * apart from the two reads it is handed.
 */
internal fun reactionNotificationBody(
    row: ProcessedRow,
    ownPersonKeyB64: String?,
    enabled: () -> Boolean,
    muted: () -> Boolean,
): String? {
    if (row.kind != ProcessedKind.REACTION || row.reactionAdd != true) return null
    val emoji = row.reactionEmoji ?: return null
    if (ownPersonKeyB64 == null || row.reactionTargetAuthorB64 != ownPersonKeyB64) return null
    if (row.senderAccountB64 == ownPersonKeyB64) return null
    if (!enabled() || muted()) return null
    // The reactor is named by the name this device holds for them. With no
    // name there is no reaction line ([notificationShape] then refuses the
    // row and the wake shows the generic banner): never a placeholder word.
    val name = row.senderName?.trim()?.takeIf { it.isNotEmpty() } ?: return ""
    return me.lakko.app.l10n.Localization.strings.value.format(
        "notification.reaction.body",
        texts = mapOf("name" to name, "emoji" to emoji),
    )
}

/**
 * The banner of a row whose sender has no name on this device: the generic
 * one a wake that cannot be read shows ("Lakko" / "New message"). A sender is
 * always a contact, a group member or a channel member whose name is stored
 * here, so this is a defect, and it is logged as one, with no identifier.
 */
internal fun unnamedSenderPreview(tag: String): PushPreview {
    try {
        android.util.Log.e("LakkoPush", "notification sender has no name on this device")
    } catch (_: RuntimeException) {
        // Host unit tests: android.util.Log is a stub.
    }
    return PushPreview(
        title = "Lakko",
        body = me.lakko.app.l10n.Localization.strings.value["notification.message.new"],
        tag = tag,
    )
}

/**
 * The sender's name for a background notification, by the rule the app's
 * screens use ([me.lakko.app.profile.ProfileDirectory.resolve], which every
 * bubble, member row and one-to-one title asks): the name filed under the
 * PERSON key (the ADR-0050 account key a leaf is bound to, which is what
 * every receive path files a profile under), else under the leaf's own
 * signature key, else `null` (no name stored: a defect the caller reports,
 * never covered by a placeholder word).
 */
internal fun backgroundSenderName(
    personKeyB64: String,
    leafKeyB64: String?,
    resolve: (String) -> me.lakko.app.profile.ResolvedName,
): String? {
    val person = resolve(personKeyB64)
    if (!person.isPlaceholder && person.primary.isNotBlank()) return person.primary
    if (leafKeyB64 != null && leafKeyB64 != personKeyB64) {
        val leaf = resolve(leafKeyB64)
        if (!leaf.isPlaceholder && leaf.primary.isNotBlank()) return leaf.primary
    }
    return null
}

/**
 * A profile envelope met in this background batch, applied to [names] for
 * the rest of the batch: filed exactly where the app files a received
 * profile (the sender's PERSON key, resolved from the leaf against the
 * roster at receipt, C-68) and under the leaf's own key, so the sender's
 * next row in the same batch is named. In memory only; the app stores the
 * same row when it polls.
 */
internal fun withBatchProfile(
    names: me.lakko.app.profile.ProfileNameSnapshot,
    roster: List<uniffi.lakko_ffi_uniffi.GroupMember>,
    senderLeaf: UInt?,
    profile: me.lakko.app.profile.PeerProfile,
    isNewer: (ULong, ULong) -> Boolean,
): me.lakko.app.profile.ProfileNameSnapshot {
    val leaf = senderLeaf ?: return names
    val member = roster.firstOrNull { it.leafIndex == leaf } ?: return names
    val encoder = java.util.Base64.getEncoder()
    val personKey = encoder.encodeToString(me.lakko.app.chat.personKeyOf(member))
    val leafKey = encoder.encodeToString(member.signaturePublicKey)
    var out = names.withBatchProfile(personKey, profile, isNewer)
    if (leafKey != personKey) out = out.withBatchProfile(leafKey, profile, isNewer)
    return out
}

/**
 * The sender's name for the leaf [senderLeaf] of [roster] (the throwaway
 * group's roster right after the apply, C-68), or `null` when the leaf is
 * not in it.
 */
internal fun backgroundSenderNameOf(
    roster: List<uniffi.lakko_ffi_uniffi.GroupMember>,
    senderLeaf: UInt?,
    resolve: (String) -> me.lakko.app.profile.ResolvedName,
): String? {
    val leaf = senderLeaf ?: return null
    val member = roster.firstOrNull { it.leafIndex == leaf } ?: return null
    val encoder = java.util.Base64.getEncoder()
    return backgroundSenderName(
        personKeyB64 = encoder.encodeToString(me.lakko.app.chat.personKeyOf(member)),
        leafKeyB64 = encoder.encodeToString(member.signaturePublicKey),
        resolve = resolve,
    )
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
 * ADR-0073 R.5 — the conversation a woken group belongs to, for everything a
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

/**
 * Whether a row's decrypt failure is this device's own message coming back: a
 * sender cannot open what it sent. The core names the refusal: openmls's
 * `ValidationError::CannotDecryptOwnMessage` reaches the client inside the
 * error text (`process_message: ValidationError(CannotDecryptOwnMessage)`).
 * It covers every row this device published (its typing, receipts and
 * commits, which the message log never holds). A text that does not match
 * (another openmls wording) only falls back to the held-id check.
 */
internal fun isOwnMessageEcho(cause: Throwable): Boolean =
    generateSequence(cause) { it.cause }.any { it.message?.contains("CannotDecryptOwnMessage") == true }

/**
 * A message notification's title and body, the same on every client: the
 * title is the conversation's name (a group's name; in a one-to-one the other
 * person's, who is the sender); in a group the body reads `Sender: text`, in
 * a one-to-one just the text. The person is always named by the name stored
 * on this device; with none (or an empty [text], a reaction line that could
 * not name its reactor) this returns `null` and the caller shows the generic
 * banner: never a placeholder word, never the conversation's name as the
 * person, never a group body without the sender. A group with no name known
 * is titled "Lakko". Media previews (an attachment line) take the same
 * shape. [prefixSender] is `false` for a line that already names the sender
 * (a reaction).
 */
public fun notificationShape(
    kind: me.lakko.app.chat.ConversationKind?,
    groupName: String?,
    senderName: String?,
    text: String,
    prefixSender: Boolean = true,
): Pair<String, String>? {
    val sender = senderName?.trim()?.takeIf { it.isNotEmpty() } ?: return null
    if (text.isEmpty()) return null
    val body = if (prefixSender) "$sender: $text" else text
    return when (kind) {
        me.lakko.app.chat.ConversationKind.Group -> {
            val title = groupName?.trim()?.takeIf { it.isNotEmpty() } ?: "Lakko"
            title to body
        }
        me.lakko.app.chat.ConversationKind.Chat -> sender to text
        // The group could not be read: the generic title, the sender before
        // the text, since nothing says whether this is a one-to-one.
        null -> "Lakko" to body
    }
}
