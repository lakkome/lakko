// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import java.util.Collections
import kotlin.coroutines.cancellation.CancellationException
import me.lakko.app.ds.DsClient
import me.lakko.app.ds.DsClientArgError
import me.lakko.app.ds.DsHttpError
import me.lakko.app.ds.DsNetworkError
import me.lakko.app.ds.FetchResult
import me.lakko.app.profile.looksLikeProfileRequestEnvelope
import me.lakko.app.push.GroupPushRouting
import uniffi.lakko_ffi_uniffi.AttachmentRef
import uniffi.lakko_ffi_uniffi.FfiException
import uniffi.lakko_ffi_uniffi.FfiGroupName
import uniffi.lakko_ffi_uniffi.FfiPinOp
import uniffi.lakko_ffi_uniffi.FfiProfile
import uniffi.lakko_ffi_uniffi.FfiPushRoutingSecret
import uniffi.lakko_ffi_uniffi.Group
import uniffi.lakko_ffi_uniffi.Identity
import uniffi.lakko_ffi_uniffi.MessageBodyKind
import uniffi.lakko_ffi_uniffi.ParsedBody
import uniffi.lakko_ffi_uniffi.ProcessedMessage
import uniffi.lakko_ffi_uniffi.committerBeaconEnvelopeLen
import uniffi.lakko_ffi_uniffi.decodeCommitterBeacon
import uniffi.lakko_ffi_uniffi.decodeProfileEnvelope
import uniffi.lakko_ffi_uniffi.decodePushRoutingSecret
import uniffi.lakko_ffi_uniffi.parseBodyEnvelope

/**
 * Application-message orchestrator on top of MLS groups.
 *
 * Mirror of `apps/web/src/chat/client.ts`, simplified for the
 * Android side:
 *
 *   * No `IdentitySecrets` indirection — the caller hands a
 *     live [Identity] handle directly (caller-managed lifetime;
 *     ChatRepository in 1.15f4 will thread the canonical
 *     Identity through).
 *   * No `onPersist` hook on this layer — MLS state persistence
 *     (mirror of the wasm `Identity.snapshotStorage` flow) lands
 *     with 1.15f3 once `Identity.snapshotStorage` is exposed on
 *     UniFFI.
 *
 * Two operations:
 *
 *   * [sendMessage] — `Group.createApplicationMessage(identity,
 *     plaintext)` → POST to `/ds/group/{group_id_hex}/messages`
 *     via [DsClient.publishGroup]. Returns the DS message id
 *     (also the cursor value).
 *
 *   * [pollMessages] — `DsClient.fetchGroup` for the group's
 *     journal → for each row, `group.processIncoming(identity,
 *     payload)` → dispatch to [IncomingChatEvent]. Application
 *     messages carry plaintext + a `kind = Application`
 *     discriminator; commits / proposals advance MLS state but
 *     surface as `System` rows the UI can render as a thin
 *     separator (or skip).
 *
 * ## Self-message echo
 *
 * `Group.createApplicationMessage` produces wire bytes that look
 * like a normal MLS application message — the DS does not
 * deduplicate, and a local poll WILL pick our own messages back
 * up. OpenMLS can't decrypt the sender's own application
 * messages at the same epoch, so it would surface as a
 * `processIncoming` failure: we'd see a "can't decrypt own
 * message" exception, swallow it, and the UI would never see
 * the row.
 *
 * Instead we **track the DS ids of our own publishes** during
 * the session and drop matches on receive. The set lives on the
 * [ChatClient] instance — swapping clients (e.g. logout) loses
 * the list; pending echoes after a swap surface as the
 * processIncoming-failed path above, which is harmless but
 * leaves a single noisy log entry per orphaned echo.
 *
 * The 1.18g flow extends this to commits as well: when
 * `ContactsClient.addMemberToGroup` broadcasts a commit, the
 * caller registers the resulting DS id via
 * [markOwnPublishedCommit] so the next poll suppresses the
 * echo. Cross-client because `ContactsClient` owns the
 * addMember orchestration but `ChatClient` owns the
 * `ownPublishedIds` registry; a thin accessor keeps the
 * surfaces independent.
 */
public class ChatClient(
    private val ds: DsClient = DsClient(),
) : AutoCloseable {

    /**
     * Rows this device published itself, so the next poll drops the echo.
     *
     * Keyed on `(journal, id)` — see [OwnEchoRegistry]. It used to be a bare set
     * of ids, and because a DS id is assigned per journal, an id published in
     * one chat swallowed the next row numbered the same in a DIFFERENT chat.
     * That is what stopped an incoming call ringing anywhere but the chat the
     * user had open.
     */
    private val ownPublished: OwnEchoRegistry = OwnEchoRegistry()

    override fun close() {
        ds.close()
    }

    /**
     * Encrypt [plaintext] for the current group epoch and post
     * the wire bytes to the DS group journal. Returns the DS
     * receipt fields.
     *
     * Caller keeps [group] alive — the [Group] handle carries
     * the MLS ratchet state; dropping it would lose every
     * subsequent message.
     *
     * ## SECURITY: persist BEFORE publish, fail-closed
     *
     * `createApplicationMessage` ADVANCES the MLS sender
     * ratchet: it consumes the current generation N (deriving
     * the AEAD key+nonce from N) and leaves the in-memory
     * OpenMLS provider at generation N+1. Because that provider
     * is in-memory, the N+1 state lives ONLY in the snapshot
     * until it is persisted to disk.
     *
     * [persistRatchet] is invoked AFTER the ratchet has advanced
     * to N+1 and BEFORE the gen-N ciphertext is published to the
     * DS. It MUST durably persist that N+1 state and THROW on any
     * failure. If it throws, this method propagates the failure
     * and does NOT publish — so the gen-N ciphertext never leaves
     * the device. Only one gen-N ciphertext can ever exist on the
     * wire, so generation N stays safe to re-consume after a
     * reload.
     *
     * Otherwise a crash (or a silently-failed persist) between
     * the publish and the persist would reload the ratchet at N,
     * and the next send would re-consume generation N — an
     * identical AEAD (key, nonce) on a different plaintext
     * (C1 XOR C2 = P1 XOR P2 plaintext leak to the zero-trust
     * server + Poly1305 tag forgery). A crash in the NEW window
     * (after [persistRatchet], before the publish) merely SKIPS
     * generation N; RFC 9420 receivers tolerate sender-ratchet
     * gaps, so there is no reuse either way. Mirror of the web
     * reference `apps/web/src/chat/client.ts`
     * (`persistDurableOrThrow` before `publishGroup`).
     *
     * @param persistRatchet durable, fail-closed persist of the
     *        post-advance (N+1) MLS provider snapshot. Required —
     *        there is deliberately no default, so a new caller
     *        cannot silently skip it and re-open the nonce-reuse
     *        window. A [CancellationException] it raises is
     *        re-thrown verbatim (structured concurrency); any
     *        other failure aborts the send.
     * @throws ChatClientArgError on empty plaintext.
     * @throws ChatError wrapping any underlying MLS / persist /
     *         DS / FFI failure (a persist failure aborts the send
     *         BEFORE the publish).
     */
    public suspend fun sendMessage(
        identity: Identity,
        group: Group,
        plaintext: ByteArray,
        persistRatchet: suspend () -> Unit,
    ): SendMessageResult =
        publishPreparedSend(prepareSend(identity, group, plaintext, persistRatchet))

    /**
     * G-37 — the PROVIDER half of [sendMessage], split out so the caller can
     * release the repository's global provider lock before the DS publish.
     *
     * Everything here touches the shared OpenMLS provider: the ratchet advance
     * (`createApplicationMessage`, N -> N+1) and the durable snapshot of the
     * resulting state. The network half is [publishPreparedSend].
     *
     * The persist-before-publish ordering is UNCHANGED and still fail-closed —
     * it is simply expressed by the split itself: [persistRatchet] runs here, and
     * the publish cannot run until this function has returned normally. A throw
     * (empty plaintext, MLS failure, persist failure) means no [PreparedSend]
     * exists, so there is nothing to publish and the gen-N ciphertext never
     * leaves the device.
     *
     * DELIBERATELY the only place the ratchet advances: a caller that wants the
     * publish outside the provider lock MUST come through here first.
     */
    public suspend fun prepareSend(
        identity: Identity,
        group: Group,
        plaintext: ByteArray,
        persistRatchet: suspend () -> Unit,
    ): PreparedSend {
        if (plaintext.isEmpty()) {
            throw ChatClientArgError("plaintext must be non-empty")
        }

        val wireBytes = try {
            group.createApplicationMessage(identity, plaintext)
        } catch (e: FfiException) {
            throw ChatError("createApplicationMessage failed", e)
        }

        val groupId = try {
            group.groupId()
        } catch (e: FfiException) {
            throw ChatError("group.groupId unavailable", e)
        }

        // SECURITY (nonce-reuse-on-crash) — PERSIST BEFORE
        // PUBLISH, FAIL-CLOSED. The N+1 ratchet state produced by
        // `createApplicationMessage` above must be durably on disk
        // before the gen-N ciphertext reaches the DS. If the
        // persist fails, ABORT — do NOT publish — so gen N stays
        // safe to re-consume after a reload. Re-throw a
        // CancellationException verbatim so a cancelled send is
        // not misreported as a persist failure (structured
        // concurrency); convert any genuine persist failure into
        // a send-aborting ChatError.
        try {
            persistRatchet()
        } catch (e: CancellationException) {
            throw e
        } catch (e: Throwable) {
            throw ChatError(
                "MLS snapshot persist failed — send aborted to prevent nonce reuse",
                e,
            )
        }

        return PreparedSend(groupId = groupId, wireBytes = wireBytes)
    }

    /**
     * G-37 — the NETWORK half of [sendMessage]: publish an already-prepared,
     * already-persisted ciphertext.
     *
     * Touches no MLS state at all — [PreparedSend] carries plain bytes, so this
     * runs OUTSIDE the repository's global provider lock and a 15 s DS publish no
     * longer blocks every other group's MLS work.
     *
     * ADR-0054 — [wake] rides the signed publish as the `x-lakko-wake` header
     * and the DS wakes the recipients after appending the row; `null` wakes
     * nobody. The client never calls push-gw to wake anyone.
     */
    public suspend fun publishPreparedSend(
        prepared: PreparedSend,
        wake: me.lakko.app.ds.DsWake? = null,
    ): SendMessageResult {
        val publish = try {
            ds.publishGroup(prepared.groupId, prepared.wireBytes, wake = wake)
        } catch (e: DsClientArgError) {
            throw ChatError("DS publish failed", e)
        } catch (e: DsHttpError) {
            // ADR-0053 — a refusal the device clock explains says so.
            throw ChatError(dsErrorMessage(e, "DS publish failed"), e)
        } catch (e: DsNetworkError) {
            throw ChatError("DS publish failed", e)
        }
        ownPublished.remember(DsClient.pseudonymHex(prepared.groupId), publish.id)

        return SendMessageResult(messageId = publish.id, createdAt = publish.createdAt)
    }

    /**
     * Register a DS message id as one we published — typically
     * a commit returned by `ContactsClient.addMemberToGroup`'s
     * broadcast step (1.18g equivalent). The next
     * [pollMessages] will see the id and drop it via the
     * [ownPublished] filter, same path
     * [sendMessage] uses for application messages.
     *
     * Without registration the sender's own [pollMessages]
     * would hit [Group.processIncoming] on its own outgoing
     * commit at the already-advanced epoch, fail, and emit a
     * phantom System row in the timeline.
     *
     * [groupIdHex] is REQUIRED, not convenience: a DS id is assigned per
     * journal, so an id alone names a row in every group at once and would
     * silence an unrelated chat's next row of that number.
     */
    public fun markOwnPublishedCommit(groupIdHex: String, messageId: Long) {
        ownPublished.remember(groupIdHex, messageId)
    }

    /**
     * Pull all rows newer than [after] for the group's journal,
     * decrypt application messages, return a list of
     * discriminated [IncomingChatEvent]s + a fresh cursor.
     *
     * Each [Group.processIncoming] mutates MLS provider storage
     * (application messages drain keys, commits replace state,
     * proposals queue). 1.15f3 wires a persist hook here so the
     * snapshot stays in sync.
     *
     * Bad frames / wrong epochs / malformed wire are silently
     * dropped — the cursor still advances so we don't
     * re-process. Same posture as the contacts inbox poll.
     */
    public suspend fun pollMessages(
        identity: Identity,
        group: Group,
        after: Long = 0,
        waitSecs: Int = 0,
    ): PollMessagesResult {
        val groupId = try {
            group.groupId()
        } catch (e: FfiException) {
            throw ChatError("group.groupId unavailable", e)
        }
        val raw = fetchGroupJournal(groupId, after = after, waitSecs = waitSecs)
        return applyFetchedMessages(
            identity,
            group,
            raw,
            after = after,
            groupIdHex = DsClient.pseudonymHex(groupId),
        )
    }

    /**
     * G-37 — the NETWORK half of [pollMessages], split out so the caller can run
     * it OUTSIDE the repository's locks.
     *
     * This touches no MLS state whatsoever: it is a plain `GET` against the DS
     * journal, and the `groupId` is a value the caller already holds (the
     * `GroupRecord` caches it), so not even the `Group` handle is read here.
     *
     * That matters because [waitSecs] is a DELIBERATE long poll — 30 s on the
     * live chat screen — and the previous shape awaited it while holding the one
     * global lock, so every send / edit / reaction / membership commit on EVERY
     * group queued behind an open chat screen's idle connection.
     *
     * The MLS half is [applyFetchedMessages], which the caller runs under the
     * lock. See `ChatRepository.pollGroup` for the composition + the guards that
     * keep the two halves consistent.
     */
    public suspend fun fetchGroupJournal(
        groupId: ByteArray,
        after: Long = 0,
        waitSecs: Int = 0,
    ): FetchResult {
        return try {
            ds.fetchGroup(groupId, after = after, waitSecs = waitSecs)
        } catch (e: DsClientArgError) {
            throw ChatError("DS fetch failed", e)
        } catch (e: DsHttpError) {
            throw ChatError(dsErrorMessage(e, "DS fetch failed"), e)
        } catch (e: DsNetworkError) {
            throw ChatError("DS fetch failed", e)
        }
    }

    /**
     * G-37 — the MLS half of [pollMessages]: apply an already-fetched batch.
     *
     * DELIBERATELY NOT `suspend`. Every statement below mutates OpenMLS provider
     * state through the shared `Group` handle and must run under the caller's
     * provider lock; making it non-suspending is a structural guarantee that no
     * network wait can ever be reintroduced into this critical section (a
     * suspending call simply would not compile here).
     *
     * [after] is the cursor the batch was fetched with, and seeds the returned
     * cursor so an empty batch does not rewind it.
     */
    public fun applyFetchedMessages(
        identity: Identity,
        group: Group,
        raw: FetchResult,
        after: Long = 0,
        /**
         * The journal these rows came from. Required because the own-echo
         * filter below is per journal: a DS id identifies a row only together
         * with the group whose journal assigned it.
         */
        groupIdHex: String,
    ): PollMessagesResult {
        val events = mutableListOf<IncomingChatEvent>()
        var cursor = after
        var touchedMlsState = false
        for (message in raw.messages) {
            cursor = message.id

            // Drop our own echoes — the DS doesn't dedupe and
            // OpenMLS can't decrypt them on the sender side at
            // the same epoch.
            //
            // Matched on THIS journal's id, never on the id alone. Ids are
            // per journal and every chat starts at 1, so an id-only match
            // dropped another chat's row of the same number — a fetched row,
            // silently discarded with the cursor advanced past it, which is
            // what kept incoming calls from ringing outside the open chat.
            if (ownPublished.consume(groupIdHex, message.id)) {
                continue
            }

            // Marked BEFORE the call, not after — see
            // [PollMessagesResult.touchedMlsState].
            touchedMlsState = true
            val processed = try {
                // ADR-0022 — the CHAT receive loop enforces the group-admin
                // role policy: an add / remove / role-change commit whose
                // committer's role does not authorise it is REFUSED (throws)
                // and skipped, never merged (fail-closed). A group with no role
                // table (legacy) behaves like the open `processIncoming`. This
                // is the chat group, not the SFU call group (the call path
                // reuses the chat exporter, not a separate Group), so the
                // call-path caveat does not apply here.
                group.processIncomingVerifyingRoles(identity, message.payload)
            } catch (_: Throwable) {
                // Bad frame, wrong epoch, malformed wire, a role-unauthorized commit
                // refused by the gate, OR a re-fetched own commit
                // (`CannotDecryptOwnMessage`, ADR-0040 §5 benign drop). Drop + advance
                // the cursor. ADR-0040 F3 (lakko-security) — this INNER per-message
                // catch is `Throwable`, not `FfiException`: `CannotDecryptOwnMessage`
                // surfaces as an `FfiException` today, but were a binding ever to
                // surface an own-commit re-fetch as a different `Throwable`, a narrow
                // catch would let it escape to the OUTER poll-loop catch (which does
                // NOT commit the batch cursor) and re-fetch the orphan row forever.
                // Mirrors the blanket iOS / web per-message drop posture. (Only the
                // per-message drop is widened — the outer poll-loop catch is unchanged.)
                //
                // The cursor still advances past it, deliberately: a row that can
                // never apply must not be able to wedge the poll, and only a
                // PERSIST failure holds the cursor (see [DurableCursorGate]). The
                // two are distinguishable because they are different operations at
                // different points in the batch.
                continue
            }
            // The roster is read HERE, one line after the apply, because that
            // is the epoch this row decrypted at — a leaf resolved any later
            // can name whoever was added into a vacated slot since. Read PER
            // ROW and never hoisted out of the loop: a Commit row in the middle
            // of this same batch renumbers leaves, and the rows after it must
            // resolve against the roster that commit produced.
            events.add(
                toEvent(
                    messageId = message.id,
                    createdAt = message.createdAt,
                    processed = processed,
                    resolveSender = { leaf -> resolveMemberIdentity(rosterOf(group), leaf) },
                    resolveLeafSignatureKey = { leaf -> resolveLeafSignatureKey(rosterOf(group), leaf) },
                ),
            )
        }

        return PollMessagesResult(
            events = events,
            cursor = cursor,
            hasMore = raw.hasMore,
            touchedMlsState = touchedMlsState,
        )
    }
}

/** Phase 3a10 — cheap byte-sniff for the
 *  `CallSignalingPayload` proto. Returns `true` when the
 *  payload opens with one of the oneof body tags.
 *
 *  Chat envelopes start with `{` (0x7B), disjoint from every tag.
 *
 *  Before the group tags were added, a group-control body fell
 *  through to `parseBodyEnvelope`, whose deliberate lossy-UTF-8
 *  fallback rendered it as a garbage chat bubble (foreground) or a
 *  garbage notification (background, which reuses this function).
 *
 *  `internal` rather than `private` ONLY so the background
 *  push-preview path can reuse this exact sniff instead of
 *  duplicating the tag set — see
 *  `me.lakko.app.push.isServiceEnvelope`.
 *
 *  The tag table itself moved to
 *  [me.lakko.app.calls.callSignalingBodyKind] when the outbound
 *  wake decision needed to tell an offer from a candidate rather
 *  than merely recognise both: the proto oneof tags must have
 *  exactly one definition, and a second copy would drift silently
 *  the next time a field number moves. This is the same predicate
 *  it always was — "is this call signaling at all". */
/** ADR-0053 client contract point 5 — the user-facing sentence of a DS
 *  failure: "Check this device's date and time." when a proof refusal is
 *  explained by the device clock, [fallback] otherwise. */
internal fun dsErrorMessage(error: Throwable, fallback: String): String =
    (error as? me.lakko.app.ds.DsProofRefusedError)?.userMessage ?: fallback

internal fun looksLikeCallSignaling(bytes: ByteArray): Boolean =
    me.lakko.app.calls.callSignalingBodyKind(bytes) != null

/** First byte of the ADR-0045 committer-beacon domain separator (`'l'` of
 *  `"lakko/committer-beacon/v1"` plus a trailing NUL, written out in words and
 *  never as a raw NUL byte). The same `'l'` every `lakko/...` in-group envelope
 *  domain opens with, so on its own it decides nothing — it only skips an FFI
 *  call for a chat envelope (which starts `{`, 0x7B). */
private const val COMMITTER_BEACON_FIRST_BYTE: Int = 0x6C

/**
 * The beacon's exact wire length, from the FFI's `committerBeaconEnvelopeLen()`
 * rather than a local `27`. That constant lives in
 * `crates/lakko-contacts/src/committer_beacon.rs` and is exported precisely so
 * the hosts do not each hand-copy the number — the drift shape ADR-0044 was
 * written about. The foreground links the full FFI, so unlike the slim NSE
 * surface there is no reason to compromise.
 *
 * `by lazy`, NOT an eager top-level `val`: a top-level initialiser runs at
 * `ChatClientKt` class-init, and this file's [looksLikeCallSignaling] is reused
 * by the background `me.lakko.app.push.isServiceEnvelope`, which runs in plain
 * JVM unit tests where `liblakko_ffi_uniffi.so` does not exist. An eager read
 * therefore turns every host test that merely touches this file into an
 * `ExceptionInInitializerError`. Deferring it means the `.so` is touched on the
 * first real classification, on-device, and never at class load.
 */
private val committerBeaconEnvelopeLength: Int by lazy {
    committerBeaconEnvelopeLen().toInt()
}

/**
 * ADR-0045 — does this application plaintext classify as a committer-liveness
 * beacon?
 *
 * Unlike the other envelope filters here, the length-plus-prefix peek is a
 * COMPLETE test rather than merely a fast path: the beacon carries no fields at
 * all, so every beacon on the wire is byte-identical and the exact length plus
 * the domain prefix already pin all but the version byte. [decodesAsBeacon] is
 * confirmation, and it is what makes the arm fail-closed on an unknown schema
 * version — a future v2 beacon this build does not understand is NOT silently
 * swallowed here; it falls through to the chat parser like any other
 * unrecognised plaintext.
 *
 * Both FFI values are PARAMETERS rather than reads inside this function, for
 * the same reason the background `isServiceEnvelope` takes them: it keeps the
 * decision free of the UniFFI `.so` so it runs in a plain JVM unit test. The
 * one real call site supplies the real FFI.
 *
 * @param bytes the decrypted MLS Application plaintext.
 * @param envelopeLen the exact on-wire beacon length, from the FFI constant at
 *   the call site — never written down here.
 * @param decodesAsBeacon the authoritative fail-closed decode (the native
 *   `decodeCommitterBeacon`, reported as a boolean). Called ONLY after the
 *   cheap peek passes, so a real chat envelope never pays for an exception.
 */
internal fun looksLikeCommitterBeacon(
    bytes: ByteArray,
    envelopeLen: Int,
    decodesAsBeacon: (ByteArray) -> Boolean,
): Boolean {
    if (bytes.size != envelopeLen || bytes.isEmpty()) return false
    if ((bytes[0].toInt() and 0xFF) != COMMITTER_BEACON_FIRST_BYTE) return false
    return decodesAsBeacon(bytes)
}

/** The authoritative beacon decode, reported as a boolean. `false` for a
 *  wrong-domain / wrong-length / unknown-version plaintext, so the caller runs
 *  the normal chat parse (fail-closed, never a silent swallow). */
private fun decodesAsCommitterBeacon(bytes: ByteArray): Boolean =
    try {
        decodeCommitterBeacon(bytes)
        true
    } catch (_: Throwable) {
        false
    }

/** The production ADR-0045 beacon arm: the cheap peek sized by the FFI
 *  constant, then the authoritative FFI decode. The default [toEvent] binding;
 *  a host test substitutes a stand-in because both halves need the `.so`. */
private fun committerBeaconArm(bytes: ByteArray): Boolean =
    looksLikeCommitterBeacon(bytes, committerBeaconEnvelopeLength, ::decodesAsCommitterBeacon)

/**
 * ADR-0057 — the registrar-seed envelope's first byte (`l` of its domain
 * separator), the cheap half of the peek; disjoint from a chat envelope's `{`.
 */
private const val REGISTRAR_SEED_FIRST_BYTE: Int = 0x6C

/** The on-wire registrar-seed envelope length, from the core, read lazily for
 *  the same reason as [committerBeaconEnvelopeLength]; `-1` (matches nothing,
 *  so the payload falls through, fail-closed) when the core is not loaded —
 *  which is also what keeps every host test that never names this arm off the
 *  native library. */
private val registrarSeedEnvelopeLength: Int by lazy {
    try {
        uniffi.lakko_ffi_uniffi.registrarSeedEnvelopeLen().toInt()
    } catch (_: Throwable) {
        -1
    }
}

/**
 * ADR-0057 obligation 4 — does this application plaintext classify as a
 * registrar-seed envelope? The exact length and the first byte, then the
 * authoritative fail-closed decode (the seed itself stays sealed). Pure over
 * its two injected facts, so a host test drives it without the `.so`; the
 * same shape as [looksLikeCommitterBeacon], and reused by the background
 * push filter so the envelope never becomes a notification body either.
 */
internal fun looksLikeRegistrarSeedEnvelope(
    bytes: ByteArray,
    envelopeLen: Int,
    decodesAsRegistrarSeed: (ByteArray) -> Boolean,
): Boolean {
    if (bytes.size != envelopeLen || bytes.isEmpty()) return false
    if ((bytes[0].toInt() and 0xFF) != REGISTRAR_SEED_FIRST_BYTE) return false
    return decodesAsRegistrarSeed(bytes)
}

/** The authoritative registrar-seed decode, reported as a boolean. */
internal fun decodesAsRegistrarSeed(bytes: ByteArray): Boolean =
    try {
        uniffi.lakko_ffi_uniffi.decodeRegistrarSeedEnvelope(bytes)
        true
    } catch (_: Throwable) {
        false
    }

/** The production registrar-seed arm (peek sized by the FFI constant, then
 *  the FFI decode). */
internal fun registrarSeedArm(bytes: ByteArray): Boolean {
    // The first byte before the length: only an `l`-prefixed payload may
    // touch the lazily-read core constant.
    if (bytes.isEmpty() || (bytes[0].toInt() and 0xFF) != REGISTRAR_SEED_FIRST_BYTE) return false
    return looksLikeRegistrarSeedEnvelope(bytes, registrarSeedEnvelopeLength, ::decodesAsRegistrarSeed)
}

/**
 * ADR-0048 — the frozen profile-envelope domain separator,
 * `lakko/profile/v1` followed by one NUL byte. Built programmatically: a raw
 * NUL in a source file makes git treat it as binary.
 */
private val PROFILE_ENVELOPE_DOMAIN: ByteArray =
    "lakko/profile/v1".toByteArray(Charsets.US_ASCII) + byteArrayOf(0)

/**
 * ADR-0048 — does this application plaintext open with the profile-envelope
 * domain separator?
 *
 * Unlike the routing-secret peek this prefix test is a complete CLASSIFICATION
 * (though not a complete decode): the 17-byte separator ends in a NUL byte, so
 * no chat envelope (`{`) and no legacy UTF-8 message a person typed can open
 * with it. A plaintext that does is a profile envelope whether or not it
 * decodes, and one that does not decode — an out-of-bounds name, a truncated
 * frame, an unknown version — is DROPPED rather than handed to the chat
 * parser, whose lossy-UTF-8 fallback would render the domain string and the
 * raw bytes as a bubble. The core refuses such an envelope "as a whole"
 * (ADR-0048 §1), and so does this client.
 *
 * `internal` so the background push-preview filter reuses this exact test.
 * Pure — host-testable.
 */
internal fun looksLikeProfileEnvelope(bytes: ByteArray): Boolean {
    if (bytes.size < PROFILE_ENVELOPE_DOMAIN.size) return false
    for (i in PROFILE_ENVELOPE_DOMAIN.indices) {
        if (bytes[i] != PROFILE_ENVELOPE_DOMAIN[i]) return false
    }
    return true
}

/** The authoritative, fail-closed profile decode: the profile, or `null` for a
 *  malformed / out-of-bounds envelope (which is then dropped, never rendered). */
private fun decodeProfileOrNull(bytes: ByteArray): FfiProfile? =
    try {
        decodeProfileEnvelope(bytes)
    } catch (_: Throwable) {
        null
    }

/** ADR-0051 — the authoritative, fail-closed group-name decode: the name, or
 *  `null` for a malformed / out-of-bounds envelope (dropped, never rendered). */
private fun decodeGroupNameOrNull(bytes: ByteArray): FfiGroupName? =
    try {
        uniffi.lakko_ffi_uniffi.decodeGroupNameEnvelope(bytes)
    } catch (_: Throwable) {
        null
    }

/** ADR-0052 decision 3 — the authoritative, fail-closed pin decode: the
 *  operation, or `null` for a malformed envelope (dropped, never rendered). */
private fun decodePinOrNull(bytes: ByteArray): FfiPinOp? =
    try {
        uniffi.lakko_ffi_uniffi.decodePinEnvelope(bytes)
    } catch (_: Throwable) {
        null
    }

/** ADR-0048 trigger 4 — the authoritative, fail-closed profile-request decode,
 *  reported as a boolean because the request carries nothing to hand back: a
 *  clean return IS the verdict. A bad length or an unknown schema version
 *  throws and yields `false`, and the request is then dropped — never
 *  answered, never rendered (the peek already classified it). */
private fun decodesAsProfileRequest(bytes: ByteArray): Boolean =
    try {
        uniffi.lakko_ffi_uniffi.decodeProfileRequest(bytes)
        true
    } catch (_: Throwable) {
        false
    }

/**
 * Classify one decrypted DS row into an [IncomingChatEvent].
 *
 * `internal`, and with its two FFI touchpoints as DEFAULTED PARAMETERS, so the
 * host unit suite can drive the whole classification chain: the UniFFI `.so`
 * is Android-only, and a plain JVM test that reached either of these would die
 * with `UnsatisfiedLinkError` rather than assert anything. Production passes
 * nothing and gets the real FFI. Same rationale — and the same shape — as the
 * background `me.lakko.app.push.isServiceEnvelope`, which takes its codecs the
 * same way for the same reason.
 *
 * @param resolveSender maps an MLS leaf index to the member's signature key,
 *   against the group's roster AS IT STANDS NOW. DELIBERATELY has no default:
 *   a `{ null }` default would let a new call site compile and then silently
 *   fail every attribution closed — edit, delete, reactions, typing and read
 *   ticks all dead, and looking like nothing was wired wrong. Both real callers
 *   invoke this function immediately after `processIncomingVerifyingRoles`
 *   returned, which is the one moment "now" is the epoch the message was sealed
 *   at (an Application frame only decrypts at the current epoch, and a Commit
 *   that renumbers leaves is a separate journal row applied before or after,
 *   never during). Resolving later, from a stored leaf, is what lets a member
 *   added into a departed member's vacated leaf inherit that member's
 *   authorship. See `AuthorIdentity.kt`.
 * @param resolveLeafSignatureKey maps an MLS leaf index to that leaf's own
 *   (device) signature key, at the same moment and for the same reason as
 *   [resolveSender]; the group-call ledger keys joined devices on it. No
 *   default, for the same reason.
 * @param isCommitterBeacon the ADR-0045 beacon arm (cheap peek + authoritative
 *   decode). Defaults to the real FFI-backed [committerBeaconArm].
 * @param parseBody the ADR-0023 structured body parser. Defaults to the real
 *   FFI `parseBodyEnvelope`. Note its documented contract, which is why the
 *   beacon needs its own arm AHEAD of it: a plaintext it does not recognise
 *   DEGRADES to [MessageBodyKind.CHAT] with lossy-UTF-8 text — it does NOT
 *   throw. Any stand-in that throws instead would make the beacon arm look
 *   unnecessary.
 */
internal fun toEvent(
    messageId: Long,
    createdAt: String,
    processed: ProcessedMessage,
    resolveSender: (UInt) -> MemberIdentity?,
    resolveLeafSignatureKey: (UInt) -> MemberIdentity?,
    isCommitterBeacon: (ByteArray) -> Boolean = ::committerBeaconArm,
    parseBody: (ByteArray) -> ParsedBody = ::parseBodyEnvelope,
    decodeProfile: (ByteArray) -> FfiProfile? = ::decodeProfileOrNull,
    decodeGroupName: (ByteArray) -> FfiGroupName? = ::decodeGroupNameOrNull,
    isProfileRequest: (ByteArray) -> Boolean = ::decodesAsProfileRequest,
    decodePin: (ByteArray) -> FfiPinOp? = ::decodePinOrNull,
    isRegistrarSeed: (ByteArray) -> Boolean = ::registrarSeedArm,
): IncomingChatEvent =
    when (processed) {
        is ProcessedMessage.Application -> {
            // ADR-0042 — sniff for a push-routing-secret distribution
            // envelope (a cheap length + first-byte pre-filter, then the
            // authoritative fail-closed FFI decode). A non-envelope
            // plaintext returns `null` and falls through to the normal
            // chat parse. First byte `'l'` (0x6c) is disjoint from both
            // the chat `{` (0x7B) and the call-signaling tags below.
            // ADR-0044 — the v2 decode yields the 32-byte secret AND the
            // `mint_epoch` the MINTER stamped it with. A retired v1
            // envelope (no stamp, hence no position in the recency order)
            // is refused by the FFI with `UnknownVersion` and lands in
            // the `null` branch — a fail-closed drop, never a
            // downgrade.
            val routingSecret: FfiPushRoutingSecret? =
                if (GroupPushRouting.looksLikeSecretEnvelope(processed.data)) {
                    try {
                        decodePushRoutingSecret(processed.data)
                    } catch (_: Throwable) {
                        null
                    }
                } else {
                    null
                }
            // Phase 3a10 — sniff for call-signaling proto
            // bytes before running the chat-envelope
            // parser. Cheap unambiguous discriminator:
            // CallSignalingPayload's first proto wire tag
            // is one of 0x52 / 0x5A / 0x62 / 0x6A / 0x72 /
            // 0x7A / 0x82 0x01 (oneof fields 10..16, see
            // [looksLikeCallSignaling]); chat envelopes start
            // with `{` (0x7B). No overlap.
            if (looksLikeCallSignaling(processed.data)) {
                IncomingChatEvent.CallSignaling(
                    messageId = messageId,
                    payloadBytes = processed.data,
                    // The MLS-authenticated sender leaf: a group-control body
                    // names a leaf of its own, and the ledger refuses one
                    // that does not match this (a member may only speak for
                    // itself).
                    senderLeaf = processed.senderLeaf,
                    // Resolved NOW, at the epoch this row decrypted at: the
                    // group-call ledger keys joined devices on this leaf's own
                    // signature key, never on the leaf index (C-68).
                    senderKey = processed.senderLeaf?.let(resolveLeafSignatureKey),
                    // ...and the person that device belongs to, the way
                    // authorship resolves it, for the "N people" count.
                    senderPersonKey = processed.senderLeaf?.let(resolveSender),
                    createdAt = createdAt,
                )
            } else if (routingSecret != null) {
                // ADR-0042 — never a timeline bubble: the repository
                // persists the secret + subscribes under the derived
                // opaque `push_gid`.
                //
                // `senderLeaf` is the MLS-authenticated author leaf of
                // this Application message (`null` for an external
                // sender). It is the ONLY trustworthy sender identity
                // available here — the envelope BODY is attacker-chosen,
                // the leaf is not — so the repository's receive gate
                // resolves it to the sender's signature key and refuses
                // a CHANGED secret from a non-owner. Without it any one
                // member could unilaterally rotate the group's push_gid,
                // forcing every member to re-subscribe and stranding an
                // orphan push-gw subscription per churn. Same threading
                // precedent as the ADR-0023 author-only edit/delete gate
                // below.
                IncomingChatEvent.PushRoutingSecret(
                    messageId = messageId,
                    secret = routingSecret.secret,
                    // ADR-0044 — the MINTER's stamp, carried through so the
                    // receive gate can order it against what is held. NOT
                    // the receiver's own epoch.
                    mintEpoch = routingSecret.mintEpoch,
                    senderLeaf = processed.senderLeaf,
                    createdAt = createdAt,
                )
            } else if (isCommitterBeacon(processed.data)) {
                // ADR-0045 decision 2 — a committer-liveness beacon: an
                // owner / admin announcing it is foregrounded and able to
                // sweep the pending proposal set. Never a timeline bubble.
                //
                // Without this arm the beacon reaches `parseBodyEnvelope`,
                // whose lossy-UTF-8 fallback degrades it (correctly, per its
                // own contract — legacy plaintext must still render) into a
                // visible bubble reading `lakko/committer-beacon/v1` plus two
                // control bytes.
                //
                // The envelope carries NO fields at all, deliberately: naming
                // the sender would add an unauthenticated identity claim next
                // to the MLS-authenticated one, and a self-reported timestamp
                // would let a candidate hold the committer duty by asserting
                // its own freshness. So the two things the takeover fold
                // needs ride along from outside the bytes — WHO, from the
                // MLS-authenticated `senderLeaf`, and HOW OLD, from the DS
                // row's `createdAt`. That is why this is surfaced as an event
                // rather than dropped here.
                IncomingChatEvent.CommitterBeacon(
                    messageId = messageId,
                    senderLeaf = processed.senderLeaf,
                    createdAt = createdAt,
                )
            } else if (me.lakko.app.reach.scopedEnvelopeKindOf(processed.data) != null) {
                // ADR-0060 — a scoped-discovery service envelope. Never a
                // timeline bubble: every domain ends in a NUL byte, so the
                // prefix is a complete classification, and one that does not
                // decode is dropped by the repository rather than rendered.
                IncomingChatEvent.ScopedDiscovery(
                    messageId = messageId,
                    kind = me.lakko.app.reach.scopedEnvelopeKindOf(processed.data)!!,
                    bytes = processed.data,
                    senderKey = processed.senderLeaf?.let(resolveSender),
                    createdAt = createdAt,
                )
            } else if (looksLikeProfileEnvelope(processed.data)) {
                // ADR-0048 — a member's profile. Never a timeline bubble.
                // The sender is resolved to its signature KEY here, one line
                // after the apply, because this is the one moment the roster
                // is the epoch the envelope was sealed at (C-68). The profile
                // names no identity of its own, so nothing in its bytes can
                // choose whom it is stored under. A malformed envelope arrives
                // as `profile = null` and is consumed as a no-op.
                IncomingChatEvent.Profile(
                    messageId = messageId,
                    profile = decodeProfile(processed.data),
                    senderKey = processed.senderLeaf?.let(resolveSender),
                    createdAt = createdAt,
                )
            } else if (looksLikeGroupNameEnvelope(processed.data)) {
                // ADR-0051 — the group's shared name. Never a timeline bubble.
                // Same shape as the profile arm: the sender is resolved to its
                // account KEY here (C-68), because the repository's role gate
                // judges the sender's authority at the moment of receipt. A
                // malformed envelope arrives as `groupName = null` and is
                // consumed as a no-op; the chat parser never sees it.
                IncomingChatEvent.GroupName(
                    messageId = messageId,
                    groupName = decodeGroupName(processed.data),
                    senderKey = processed.senderLeaf?.let(resolveSender),
                    createdAt = createdAt,
                )
            } else if (looksLikePinEnvelope(processed.data)) {
                // ADR-0052 decision 3 — a shared pin or unpin of one message.
                // Never a timeline bubble. Same shape as the group-name arm:
                // the sender is resolved to its account KEY here (C-68),
                // because the repository's gate judges the sender's standing
                // at the moment of receipt and orders the action by its stamp
                // against what is stored for that message. A malformed
                // envelope arrives as `pin = null` and is consumed as a no-op;
                // the chat parser never sees it.
                IncomingChatEvent.Pin(
                    messageId = messageId,
                    pin = decodePin(processed.data),
                    senderKey = processed.senderLeaf?.let(resolveSender),
                    createdAt = createdAt,
                )
            } else if (looksLikeProfileRequestEnvelope(processed.data)) {
                // ADR-0048 trigger 4 — a member asking the group to introduce
                // itself. Never a timeline bubble. The request carries no
                // fields, so the only thing about it worth knowing is WHO
                // asked, resolved to the account key here, at the epoch the
                // row decrypted at (C-68): the repository answers with this
                // device's own profile through the growth path unless the
                // asker is this same person on another device. A malformed
                // request arrives as `wellFormed = false` and is consumed as a
                // no-op — dropped, never answered, never handed to the chat
                // parser whose lossy-UTF-8 fallback would render it.
                IncomingChatEvent.ProfileRequest(
                    messageId = messageId,
                    wellFormed = isProfileRequest(processed.data),
                    senderKey = processed.senderLeaf?.let(resolveSender),
                    createdAt = createdAt,
                )
            } else if (isRegistrarSeed(processed.data)) {
                // ADR-0057 obligation 4 — the owner handing ONE member its
                // registrar seed, sealed to that member's account key. Never a
                // timeline bubble. The sender is resolved to its account KEY
                // here (C-68): the repository keeps the seed only when that key
                // is the owner's in the live role table and the envelope names
                // this account; the envelope names no sender of its own.
                IncomingChatEvent.RegistrarSeed(
                    messageId = messageId,
                    envelope = processed.data,
                    senderKey = processed.senderLeaf?.let(resolveSender),
                    createdAt = createdAt,
                )
            } else {
                // ADR-0023 — feed the application bytes through
                // the structured body parser, which surfaces the
                // chat / edit / delete / reaction discriminator.
                // The plaintext was already authenticated by the
                // MLS Application message's MAC, so the worst case
                // here is a malformed envelope from a buggy peer —
                // never a tampered payload. A malformed / ambiguous
                // envelope degrades to CHAT with the raw text;
                // legacy pre-1.19 plain-UTF-8 bytes collapse to
                // `(text = <utf-8 lossy>, attachments = [])`.
                //
                // `senderLeaf` is the ORIGINAL author's MLS leaf for
                // this Application message (`null` for an external
                // sender — such a message can never be a valid
                // edit/delete target). It feeds the author-only
                // edit/delete gate + reaction attribution.
                val body = parseBody(processed.data)
                val senderLeaf = processed.senderLeaf
                // ...and the IDENTITY that leaf names, resolved HERE because
                // here is where the roster is the one the message was sealed
                // under. Every downstream attribution reads this, not the leaf.
                val senderKey = senderLeaf?.let(resolveSender)
                when (body.kind) {
                    MessageBodyKind.CHAT -> IncomingChatEvent.Application(
                        messageId = messageId,
                        text = body.text,
                        attachments = body.attachments,
                        plaintext = body.text,
                        senderLeaf = senderLeaf,
                        senderKey = senderKey,
                        createdAt = createdAt,
                        // ADR-0052 — a chat message that also points somewhere.
                        // The core sets the three fields only for `CHAT`; the
                        // snapshot is carried verbatim, never resolved here.
                        reply = body.replyTo?.let { target ->
                            ReplyRef(
                                target = target,
                                author = body.replyAuthor,
                                excerpt = body.replyExcerpt,
                            )
                        },
                        // ADR-0052 decision 2 — the forward attribution, only
                        // for `CHAT`. The core hands the origin pair both or
                        // neither; the group id arrives as standard base64 and
                        // is keyed here as the hex the app names groups by. A
                        // pair whose id does not decode is dropped as a whole,
                        // never half-kept.
                        forward = forwardRefFrom(
                            forwardedFrom = body.forwardedFrom,
                            originGroupB64 = body.forwardOriginGroupB64,
                            originMessageId = body.forwardOriginMessageId,
                        ),
                    )
                    MessageBodyKind.EDIT -> IncomingChatEvent.Op(
                        messageId = messageId,
                        op = MessageOp.Edit,
                        target = body.target ?: 0L,
                        text = body.text,
                        emoji = null,
                        senderLeaf = senderLeaf,
                        senderKey = senderKey,
                        createdAt = createdAt,
                    )
                    MessageBodyKind.DELETE -> IncomingChatEvent.Op(
                        messageId = messageId,
                        op = MessageOp.Delete,
                        target = body.target ?: 0L,
                        text = "",
                        emoji = null,
                        senderLeaf = senderLeaf,
                        senderKey = senderKey,
                        createdAt = createdAt,
                    )
                    MessageBodyKind.REACTION -> IncomingChatEvent.Op(
                        messageId = messageId,
                        op = MessageOp.Reaction,
                        target = body.target ?: 0L,
                        text = "",
                        emoji = body.emoji,
                        senderLeaf = senderLeaf,
                        senderKey = senderKey,
                        createdAt = createdAt,
                    )
                    // ADR-0024 — an ephemeral typing signal. Never a
                    // bubble; the repository folds it into the per-leaf
                    // typing state (with a stale-`createdAt` re-sync
                    // guard + auto-expiry). `senderLeaf` attributes it
                    // (an unattributable signal is dropped on apply).
                    MessageBodyKind.TYPING -> IncomingChatEvent.Typing(
                        messageId = messageId,
                        isTyping = body.isTyping ?: false,
                        senderLeaf = senderLeaf,
                        senderKey = senderKey,
                        createdAt = createdAt,
                    )
                    // ADR-0024 — a read receipt: the sender has read up
                    // to `target` (the read-up-to messageId watermark).
                    // The repository folds it into the monotone
                    // per-leaf `readUpTo` map.
                    MessageBodyKind.READ_RECEIPT -> IncomingChatEvent.ReadReceipt(
                        messageId = messageId,
                        upTo = body.target ?: 0L,
                        senderLeaf = senderLeaf,
                        senderKey = senderKey,
                        createdAt = createdAt,
                    )
                    // A `lakko/`-domain envelope none of the arms above
                    // recognised: an unknown kind, or a known domain whose body
                    // this build cannot decode (a newer version from a client
                    // on another build). The core classifies it instead of
                    // degrading it to lossy-UTF-8 chat text, and it is DROPPED:
                    // never a bubble, never a chat-list preview line.
                    MessageBodyKind.SERVICE_ENVELOPE -> IncomingChatEvent.ServiceEnvelope(
                        messageId = messageId,
                        createdAt = createdAt,
                    )
                }
            }
        }
        is ProcessedMessage.Commit -> IncomingChatEvent.System(
            messageId = messageId,
            systemKind = SystemKind.Commit,
            createdAt = createdAt,
        )
        is ProcessedMessage.Proposal -> IncomingChatEvent.MembershipProposal(
            // ADR-0045 UX §10 — a PANEL signal carrying its record, never a
            // content-less system row. The record is what makes an
            // observation possible at all: `proposalKey` needs the kind and
            // its target, and without a key there is nothing to stamp.
            messageId = messageId,
            record = processed.record,
            createdAt = createdAt,
        )
        is ProcessedMessage.ExternalJoinProposal -> IncomingChatEvent.ExternalJoinRequest(
            // ADR-0027 (G-3) — surface the joiner's signature key the members
            // sheet needs to approve / reject, not a content-less system row.
            messageId = messageId,
            joinerSignatureKey = processed.joinerSignatureKey,
            retained = processed.retained,
            createdAt = createdAt,
        )
    }

// =============================================================================
// Result types
// =============================================================================

/**
 * G-37 — a sealed, already-persisted application message awaiting only its DS
 * publish. The hand-off between [ChatClient.prepareSend] (provider work, runs
 * under the repository's global provider lock) and
 * [ChatClient.publishPreparedSend] (network, runs outside it).
 *
 * Holds plain bytes only — no `Group`, no `Identity`, no provider handle — which
 * is exactly what makes it safe to carry across the lock release.
 */
public class PreparedSend internal constructor(
    /** Raw MLS group id the ciphertext is addressed to. */
    internal val groupId: ByteArray,
    /** The gen-N application-message wire bytes, already sealed. */
    internal val wireBytes: ByteArray,
)

/** Result of [ChatClient.sendMessage]. */
public data class SendMessageResult(
    /** DS-assigned monotonic id (also the cursor value for the
     *  next [ChatClient.pollMessages] call). */
    val messageId: Long,
    /** ISO-8601 timestamp from the DS. */
    val createdAt: String,
)

/** Discriminated chat event surfaced by [ChatClient.pollMessages]. */
public sealed interface IncomingChatEvent {
    /** Application-data message (user-visible chat row).
     *
     *  Phase 1.19c4: now carries `(text, attachments)` from the
     *  1.19a JSON envelope (parsed via the UniFFI
     *  `parseEnvelope(bytes:)` surface from 1.19b). Legacy
     *  pre-1.19 plain-UTF-8 bytes collapse cleanly to
     *  `(text = <utf-8 lossy>, attachments = [])` via the
     *  parser's documented fallback paths — existing bubbles
     *  render unchanged. `plaintext` is preserved as an alias
     *  of `text` for transitional call-site compatibility. */
    public data class Application(
        val messageId: Long,
        /** Plain user-visible text from the envelope (empty
         *  string is valid — attachment-only message). */
        val text: String,
        /** ADR-0052 — the reply reference and quote, when the envelope
         *  carried `reply_to`; `null` for an ordinary message. */
        val reply: ReplyRef? = null,
        /** ADR-0052 decision 2 — the forward attribution, when the envelope
         *  carried `forwarded_from` or an origin; `null` otherwise. */
        val forward: ForwardRef? = null,
        /** Zero or more attachment refs from the envelope. */
        val attachments: List<AttachmentRef>,
        /** Alias of [text] — kept for the few call sites that
         *  still read `plaintext`. New code should read
         *  [text]. */
        val plaintext: String,
        /** ADR-0023 — the original author's MLS leaf index. Kept for
         *  diagnostics and for the roster lookups that legitimately want a
         *  POSITION; it is NOT what authorship is compared on (see
         *  [senderKey]). */
        val senderLeaf: UInt?,
        /** ADR-0023 — the original author's IDENTITY: their signature public
         *  key, base64, resolved from [senderLeaf] against the roster of the
         *  epoch this message decrypted at. This is what the author-only
         *  edit/delete gate compares, and what the timeline persists.
         *
         *  A leaf index is a position in the ratchet tree, not a person:
         *  OpenMLS hands a vacated leaf to the next member added, so a leaf
         *  recorded now can name somebody else after the next commit.
         *  Resolving in [toEvent] — at receipt, in the epoch the roster is
         *  authoritative for — is the only moment the mapping is valid.
         *
         *  `null` for a frame whose sender the client could not resolve (a
         *  control message, an external sender, a torn roster read) — such a
         *  message can never be a valid edit/delete target. */
        val senderKey: MemberIdentity?,
        val createdAt: String,
    ) : IncomingChatEvent

    /** ADR-0023 — an edit / delete / reaction op carried in the
     *  same JSON envelope as a chat message. The repository
     *  folds it into the *target* message (keyed on [target] ==
     *  the target's DS [Application.messageId]) rather than
     *  rendering it as a standalone bubble. The op itself still
     *  consumes a DS [messageId] + surfaces here so the
     *  incremental poll applies it exactly once. */
    public data class Op(
        /** This op message's own DS-assigned id. */
        val messageId: Long,
        /** Which op this is. */
        val op: MessageOp,
        /** The DS [Application.messageId] of the target message. */
        val target: Long,
        /** Edit text (empty for delete / reaction). */
        val text: String,
        /** Reaction emoji (`null` outside [MessageOp.Reaction]). */
        val emoji: String?,
        /** The op sender's MLS leaf index. Diagnostic only — every
         *  attribution decision reads [senderKey]. */
        val senderLeaf: UInt?,
        /** The op sender's IDENTITY: their signature public key, base64,
         *  resolved from [senderLeaf] against the roster of the epoch this op
         *  decrypted at. The author-only gate (edit / delete) checks it
         *  against the target's stored author identity, and a reaction
         *  attributes its toggle to it — neither of which may key on a leaf,
         *  which the next member added inherits.
         *  `null` ⇒ the op is dropped (fail-closed). */
        val senderKey: MemberIdentity?,
        val createdAt: String,
    ) : IncomingChatEvent

    /** ADR-0024 — an ephemeral typing signal carried in the same
     *  JSON envelope. Never rendered as a bubble: the repository
     *  folds it into the per-`(group, senderLeaf)` "is typing"
     *  state with a stale-`createdAt` re-sync guard + auto-expiry.
     *  Consumes a DS [messageId] like every other op so the
     *  incremental poll applies it exactly once. */
    public data class Typing(
        /** This signal's own DS-assigned id. */
        val messageId: Long,
        /** `true` ⇒ the sender is typing now; `false` ⇒ an explicit
         *  stop (blur / on-send). */
        val isTyping: Boolean,
        /** The signal sender's MLS leaf index. Diagnostic only — the fold
         *  keys on [senderKey]. */
        val senderLeaf: UInt?,
        /** The signal sender's IDENTITY, resolved at receipt. The typing
         *  registry is keyed on it: a leaf key lets a newcomer at a departed
         *  member's vacated leaf inherit — and clear — their typing state.
         *  `null` ⇒ dropped (no attribution possible). */
        val senderKey: MemberIdentity?,
        /** DS row timestamp — the receiver ignores any typing
         *  signal older than the expiry window (re-sync replay
         *  guard). */
        val createdAt: String,
    ) : IncomingChatEvent

    /** ADR-0024 — a read receipt: the sender has read up to [upTo]
     *  (the highest read messageId) in this group. The repository
     *  folds it into a monotone per-`(group, senderLeaf)` watermark
     *  (a lower value never overwrites a higher one, so re-sync
     *  replay is idempotent). The message author renders read-state
     *  on its OWN messages from the other members' watermarks. */
    public data class ReadReceipt(
        /** This receipt's own DS-assigned id. */
        val messageId: Long,
        /** The read-up-to messageId watermark the sender asserts. */
        val upTo: Long,
        /** The receipt sender's MLS leaf index. Diagnostic only — the
         *  watermark keys on [senderKey]. */
        val senderLeaf: UInt?,
        /** The receipt sender's IDENTITY, resolved at receipt. The watermark
         *  is monotone and is never reset on a membership change, so a leaf
         *  key handed a departed member's whole read history to whoever was
         *  added into their slot. `null` ⇒ dropped. */
        val senderKey: MemberIdentity?,
        val createdAt: String,
    ) : IncomingChatEvent

    /** A service envelope the core recognised by its `lakko/` domain but
     *  that no arm here decodes (an unknown kind, or a version this build
     *  cannot read). Machine-to-machine: consumed as a no-op, never a
     *  timeline row and never a preview. */
    public data class ServiceEnvelope(
        val messageId: Long,
        val createdAt: String,
    ) : IncomingChatEvent

    /** Commit / external-join-proposal frame. MLS state advanced, no
     *  user-visible bytes.
     *
     *  A membership *proposal* is deliberately NOT one of these any more —
     *  ADR-0045 UX §10: it *"stops being a **timeline** row and becomes a
     *  **panel** signal"*. See [MembershipProposal]. */
    public data class System(
        val messageId: Long,
        val systemKind: SystemKind,
        val createdAt: String,
    ) : IncomingChatEvent

    /** Phase 3a10 — call-signaling event lifted out of an
     *  MLS application message. The chat repository
     *  dispatches it into the [me.lakko.app.calls.CallSession]
     *  state machine instead of the chat timeline. */
    public data class CallSignaling(
        val messageId: Long,
        val payloadBytes: ByteArray,
        val createdAt: String,
        /** MLS-authenticated sender leaf of the row, `null` for a non-member
         *  sender. A group-control body is refused unless the leaf it claims
         *  equals this one. */
        val senderLeaf: UInt? = null,
        /** The sender leaf's own (device) signature key resolved from
         *  [senderLeaf] at receipt ([resolveLeafSignatureKey]), `null` when it
         *  did not resolve. */
        val senderKey: MemberIdentity? = null,
        /** The sender's person key resolved from [senderLeaf] at receipt
         *  ([resolveMemberIdentity]), `null` when it did not resolve. */
        val senderPersonKey: MemberIdentity? = null,
    ) : IncomingChatEvent {
        // Override equals / hashCode for the payload
        // ByteArray — default reference equality would
        // surprise downstream consumers.
        override fun equals(other: Any?): Boolean {
            if (this === other) return true
            if (other !is CallSignaling) return false
            return messageId == other.messageId &&
                payloadBytes.contentEquals(other.payloadBytes) &&
                createdAt == other.createdAt &&
                senderLeaf == other.senderLeaf &&
                senderKey == other.senderKey &&
                senderPersonKey == other.senderPersonKey
        }

        override fun hashCode(): Int {
            var result = messageId.hashCode()
            result = 31 * result + payloadBytes.contentHashCode()
            result = 31 * result + createdAt.hashCode()
            result = 31 * result + (senderLeaf?.hashCode() ?: 0)
            result = 31 * result + (senderKey?.hashCode() ?: 0)
            result = 31 * result + (senderPersonKey?.hashCode() ?: 0)
            return result
        }
    }

    /** ADR-0048 — a member's profile envelope. Never a timeline bubble.
     *
     *  [profile] is `null` when the envelope matched the profile domain but was
     *  malformed or out of bounds; the core refuses such an envelope as a
     *  whole, so it is consumed as a no-op and never rendered.
     *
     *  [senderKey] is the sender's signature key resolved from the MLS sender
     *  leaf at receipt (C-68), `null` when the sender could not be resolved —
     *  an unattributable profile is never stored. */
    public data class Profile(
        val messageId: Long,
        val profile: FfiProfile?,
        val senderKey: MemberIdentity?,
        val createdAt: String,
    ) : IncomingChatEvent

    /** ADR-0027 (G-3) — an external-join *request*: a non-member posted an
     *  external Add proposal to this group's journal and the MLS core retained
     *  it pending an owner/admin approval. Carries the joiner's leaf signature
     *  key (the `proposalRef` the approve / reject repository methods take). Not
     *  a timeline bubble — the view model folds it into a pending list rendered
     *  in the members sheet (the retained set, `group.pendingExternalJoins()`,
     *  is the source of truth; this event is the nudge a new request landed). */
    public data class ExternalJoinRequest(
        val messageId: Long,
        val joinerSignatureKey: ByteArray,
        /**
         * ADR-0027 — whether the core actually KEPT the request.
         *
         * Carried rather than dropped, and the distinction is not cosmetic:
         * the core fires this event even when the quarantine
         * (`MAX_PENDING_EXTERNAL_JOINS`) was full and the request was not
         * retained. Such a request can be approved by nobody —
         * `approveExternalJoin` has nothing to look up and refuses with
         * `NO_SUCH_JOIN_REQUEST` — so offering Approve for it is an affordance
         * that cannot work. Route it through `externalJoinAffordance` rather
         * than reading this directly.
         */
        val retained: Boolean,
        val createdAt: String,
    ) : IncomingChatEvent {
        override fun equals(other: Any?): Boolean {
            if (this === other) return true
            if (other !is ExternalJoinRequest) return false
            return messageId == other.messageId &&
                joinerSignatureKey.contentEquals(other.joinerSignatureKey) &&
                retained == other.retained &&
                createdAt == other.createdAt
        }

        override fun hashCode(): Int {
            var result = messageId.hashCode()
            result = 31 * result + joinerSignatureKey.contentHashCode()
            result = 31 * result + retained.hashCode()
            result = 31 * result + createdAt.hashCode()
            return result
        }
    }

    /**
     * ADR-0045 UX §10 — a membership *request* another member published: an
     * MLS proposal the receipt gate has already judged, retained (or refused)
     * by the core's own pending set.
     *
     * **Never a timeline bubble**, and the reason is stronger than tidiness.
     * §10: *"A request is not something that happened to the group; it is an
     * intent that may never take effect. Writing 'Alice asked to remove Bob'
     * into a permanent, mutually-visible transcript is both the §10 leak and a
     * durable record of an intent that the system may discard a second
     * later."* The members sheet renders it instead, role-gated, and
     * `Group.pendingProposals()` stays the source of truth for *what* is
     * pending — this event carries *when it landed*, which the core cannot
     * know and only the DS row can say.
     *
     * A `record.stored == false` record is the receipt gate's refusal reported
     * as data rather than as a throw (a proposal advances no state, so
     * dropping it unstored *is* the refusal). It is surfaced, and recorded
     * nowhere.
     *
     * Equality is the generated `ProposalRecord`'s, which for an `Add` holds a
     * `ByteArray` and so compares by reference. Nothing folds these by value —
     * [me.lakko.app.chat.ChatRepository.consumeEvents] dispatches on the type
     * — and tests assert on fields rather than on whole-event equality.
     */
    public data class MembershipProposal(
        val messageId: Long,
        val record: uniffi.lakko_ffi_uniffi.ProposalRecord,
        val createdAt: String,
    ) : IncomingChatEvent

    /** ADR-0042 — a push-routing-secret distribution envelope: an
     *  in-group MLS application message carrying the 32-byte
     *  `push_routing_secret` the group OWNER minted. Never a timeline
     *  bubble — the repository persists it + subscribes under the
     *  derived opaque `push_gid` (closing the invariant-#5 push
     *  social-graph leak). Consumes a DS [messageId] like any other
     *  application row so the poll cursor advances past it.
     *
     *  [senderLeaf] is the MLS-authenticated author leaf (`null` for an
     *  external sender). The repository resolves it against the live
     *  roster + the ADR-0022 role table and REFUSES a changed secret
     *  from a non-owner — only the owner may rotate a group's
     *  `push_gid`. It is carried on the event because the classifier is
     *  the only place the `ProcessedMessage.Application.senderLeaf`
     *  binding is still in hand.
     *
     *  ADR-0044 — [PushRoutingSecret.mintEpoch] is the `Group.epoch()`
     *  value the MINTER stamped this secret with, carried verbatim off
     *  the v2 envelope. The receive gate orders envelopes by
     *  `(mintEpoch, secretBytes)` and clamps the stamp against the
     *  receiver's OWN epoch, which is what refuses a stale re-broadcast
     *  of a superseded secret (`lakko-security` F-A, G-36 residual 9). */
    public data class PushRoutingSecret(
        val messageId: Long,
        val secret: ByteArray,
        val mintEpoch: ULong,
        val senderLeaf: UInt?,
        val createdAt: String,
    ) : IncomingChatEvent {
        override fun equals(other: Any?): Boolean {
            if (this === other) return true
            if (other !is PushRoutingSecret) return false
            return messageId == other.messageId &&
                secret.contentEquals(other.secret) &&
                mintEpoch == other.mintEpoch &&
                senderLeaf == other.senderLeaf &&
                createdAt == other.createdAt
        }

        override fun hashCode(): Int {
            var result = messageId.hashCode()
            result = 31 * result + secret.contentHashCode()
            result = 31 * result + mintEpoch.hashCode()
            result = 31 * result + (senderLeaf?.hashCode() ?: 0)
            result = 31 * result + createdAt.hashCode()
            return result
        }
    }

    /** ADR-0045 decision 2 — a committer-liveness beacon: an owner / admin
     *  announcing it is foregrounded and able to sweep the pending proposal
     *  set. Never a timeline bubble. Consumes a DS [messageId] like any other
     *  application row so the poll cursor advances past it.
     *
     *  The envelope carries NO fields at all (a fixed 27 bytes, identical on
     *  every call and for every sender), deliberately: naming the sender would
     *  add an unauthenticated identity claim next to the MLS-authenticated
     *  one, and a self-reported timestamp would let a candidate hold the
     *  committer duty by asserting its own freshness. So everything the
     *  takeover fold needs comes from the two properties here — WHO, from the
     *  MLS-authenticated [senderLeaf], and HOW OLD, from the DS row's
     *  [createdAt] — which is exactly why this is surfaced as an event rather
     *  than dropped inside the classifier. The fold itself is not built yet;
     *  this variant is the seam it will consume.
     *
     *  [senderLeaf] is `null` for a control frame / external sender, which the
     *  fold must treat as unusable (fail-closed — an unattributable beacon
     *  proves no candidate is live).
     *
     *  A plain `data class` with no custom `equals` / `hashCode`: unlike
     *  [PushRoutingSecret] it carries no [ByteArray], so the generated
     *  structural ones are already correct. */
    public data class CommitterBeacon(
        val messageId: Long,
        val senderLeaf: UInt?,
        val createdAt: String,
    ) : IncomingChatEvent

    /** ADR-0051 — a group-name envelope. Never a timeline bubble.
     *
     *  [groupName] is `null` when the envelope matched the group-name domain
     *  but was malformed or out of bounds; it is consumed as a no-op.
     *
     *  [senderKey] is the sender's account key resolved from the MLS sender
     *  leaf at receipt (C-68), `null` when unresolvable — the repository's role
     *  gate then refuses it in a role-table group. */
    public data class GroupName(
        val messageId: Long,
        val groupName: FfiGroupName?,
        val senderKey: MemberIdentity?,
        val createdAt: String,
    ) : IncomingChatEvent

    /** ADR-0052 decision 3 — a shared pin / unpin envelope. Never a timeline
     *  bubble.
     *
     *  [pin] is `null` when the envelope matched the pin domain but was
     *  malformed; it is consumed as a no-op.
     *
     *  [senderKey] is the sender's account key resolved from the MLS sender
     *  leaf at receipt (C-68), `null` when unresolvable — the repository's gate
     *  then refuses it wherever the sender's identity decides. */
    public data class Pin(
        val messageId: Long,
        val pin: FfiPinOp?,
        val senderKey: MemberIdentity?,
        val createdAt: String,
    ) : IncomingChatEvent

    /** ADR-0057 obligation 4 — a registrar-seed envelope (the owner appointing
     *  one member as a registrar of the group's journal). Never a timeline
     *  bubble. [envelope] is the whole plaintext (the seed stays sealed until
     *  the repository's gate opens it); [senderKey] is the sender's account
     *  key resolved at receipt (C-68). */
    public class RegistrarSeed(
        public val messageId: Long,
        public val envelope: ByteArray,
        public val senderKey: MemberIdentity?,
        public val createdAt: String,
    ) : IncomingChatEvent

    /** ADR-0048 trigger 4 — a member's "introduce yourselves" request. Never a
     *  timeline bubble. Consumes a DS [messageId] like any other application
     *  row so the poll cursor advances past it.
     *
     *  [wellFormed] is `false` when the plaintext opened with the request
     *  domain but the core refused the payload (a bad length, an unknown
     *  version); such a request is consumed as a no-op — dropped, never
     *  answered.
     *
     *  [senderKey] is the asker's account key resolved from the MLS sender leaf
     *  at receipt (C-68), `null` when unresolvable — an unattributable request
     *  is not answered, since the answer would go to nobody this device can
     *  name as a member. It is also what stops this person answering their own
     *  request from another device. */
    public data class ProfileRequest(
        val messageId: Long,
        val wellFormed: Boolean,
        val senderKey: MemberIdentity?,
        val createdAt: String,
    ) : IncomingChatEvent

    /** ADR-0060 — one of the scoped-discovery service envelopes
     *  (introduction offer / bundle, committer delegation; a Visibility or
     *  ReachState that reached a conversation is dropped by the repository). Never a timeline row. [senderKey] is the
     *  sender's account key resolved at receipt (C-68); [bytes] the whole
     *  plaintext, decoded by the repository. */
    public class ScopedDiscovery(
        public val messageId: Long,
        public val kind: me.lakko.app.reach.ScopedEnvelopeKind,
        public val bytes: ByteArray,
        public val senderKey: MemberIdentity?,
        public val createdAt: String,
    ) : IncomingChatEvent
}

/**
 * Kind discriminator for [IncomingChatEvent.System].
 *
 * There is deliberately no `Proposal` entry. `System` is the only variant
 * [me.lakko.app.chat.ChatRepository.consumeEvents] turns into a
 * [ChatMessage], so an enum that cannot name a proposal is the strongest
 * available statement of ADR-0045 UX §10's rule — a membership request cannot
 * reach the transcript by any route, including one a later change adds
 * without reading §10. See [IncomingChatEvent.MembershipProposal].
 */
public enum class SystemKind {
    Commit,
    ExternalJoinProposal,
}

/** ADR-0023 — op discriminator for [IncomingChatEvent.Op]. */
public enum class MessageOp {
    Edit,
    Delete,
    Reaction,
}

/** Result of [ChatClient.pollMessages]. */
public data class PollMessagesResult(
    val events: List<IncomingChatEvent>,
    /** Fresh cursor — pass back as [ChatClient.pollMessages]'s
     *  `after` argument. */
    val cursor: Long,
    /** `true` if the DS reported more rows past the last
     *  returned id; caller should poll again immediately with
     *  the new cursor. */
    val hasMore: Boolean,
    /**
     * `true` when at least one row in this batch was HANDED to the MLS core.
     *
     * Deliberately NOT "at least one row applied successfully". A throw is no
     * proof that nothing durable changed: `process_incoming_verifying_roles`
     * runs `process_message` — which decrypts and ratchets — BEFORE the ADR-0022
     * role gate can refuse a commit or the ADR-0045 receipt gate can drop a
     * proposal. Rows the echo filter dropped never reach the core, so they do
     * not set it.
     *
     * The caller must not advance the journal cursor past such a batch until the
     * provider snapshot has durably landed — see [DurableCursorGate].
     */
    val touchedMlsState: Boolean = false,
)

// =============================================================================
// Errors
// =============================================================================

/** Programmer error caught before any network call. */
public class ChatClientArgError(message: String) : IllegalArgumentException(message)

/** Wraps any underlying failure (DS, MLS process, FFI). The
 *  cause carries the original exception for diagnostics; the message is for
 *  the log. [userText] is the sentence the screen shows when this failure
 *  has one of its own (ADR-0061 section 6), else the action's generic key. */
public class ChatError(
    message: String,
    cause: Throwable,
    public val userText: me.lakko.app.l10n.UiText? = null,
) : RuntimeException(message, cause)

