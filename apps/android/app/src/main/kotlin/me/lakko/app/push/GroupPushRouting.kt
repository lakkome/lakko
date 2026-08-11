// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.push

import android.content.Context
import java.security.SecureRandom
import java.util.concurrent.ConcurrentHashMap
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import uniffi.lakko_ffi_uniffi.composePushRoutingSecret
import uniffi.lakko_ffi_uniffi.derivePushGid as ffiDerivePushGid

/**
 * ADR-0042 orchestration — mint / receive / derive / subscribe the
 * opaque per-group push routing id (`push_gid`), closing the
 * invariant-#5 push social-graph leak.
 *
 * Mirror of `apps/web/src/push/group-routing.ts`.
 *
 * ## The load-bearing rule: OWNER mints, everyone else RECEIVES
 *
 * `push_routing_secret` is 32 RANDOM bytes, so two members minting
 * independently would diverge and break routing (their `push_gid`s
 * would differ and the notify JOIN would never match). Therefore
 * exactly ONE source mints:
 *
 *   - Only the ADR-0022 group **OWNER** mints, once, persists it,
 *     and broadcasts the secret to the group over an E2EE MLS
 *     application message (the DS sees ciphertext — invariant #2).
 *     See [ensure].
 *   - Everyone else ONLY receives + persists the secret from that
 *     envelope ([receive]) — they never mint.
 *
 * Because that broadcast is the ONLY way the rest of the group ever
 * learns the secret, its delivery is tracked durably: a row is stored
 * flagged NOT distributed until a broadcast is confirmed, and [ensure]
 * re-sends a still-pending one on every later call. Without that, one
 * failed broadcast (offline, DS error) left the owner's mint guard
 * permanently satisfied and cost the whole group push for good — see
 * [decideEnsureAction].
 *
 * On the RECEIVE side that mint authority is mirrored by a rank gate
 * ([decideAcceptReceivedSecret]): the owner may always rotate, an
 * ADR-0022 admin may only SEED a device that holds nothing (so a member
 * added by a non-owner admin still gets routing), and a plain member may
 * only relay a byte-identical secret.
 *
 * ## Never subscribe under the leaky `group_id`
 *
 * The whole point of the ADR is that push-gw + APNs/FCM must not see
 * the DS journal key. So a member that does NOT yet hold the secret
 * **defers** its push subscription ([subscribeOrDefer] returns
 * [SubscribeOutcome.DEFERRED]) — it never falls back to subscribing
 * under `group_id`. The publisher always holds the secret (it is a
 * member), so notify always has a `push_gid`.
 *
 * ## Concurrency
 *
 * Provisioning can be triggered from two places in the same session
 * — the legacy-sweep at bootstrap and the create / join path. A
 * per-`(account, group)` in-flight [Mutex] serialises them so the
 * random secret is minted exactly once (a second concurrent mint
 * would overwrite + double-broadcast a DIFFERENT secret).
 *
 * The pure orchestration operates over the [PushRoutingPorts] seam
 * (mirroring the ADR-0041 `PushDecryptDeps` split) so it is
 * host-unit-testable without the Android framework / UniFFI native
 * `.so`; [NativePushRoutingPorts] binds the real stores + FFI +
 * push-gw client.
 */
public object GroupPushRouting {

    /** Total distribution-envelope length (bytes): the 29-byte
     *  domain separator + 1 version byte + the 32-byte secret.
     *  Mirrors `lakko_contacts::push_routing` (29 + 1 + 32). Used
     *  only as a cheap receive-side pre-filter before the
     *  authoritative FFI decode. */
    public const val ENVELOPE_LEN: Int = 62

    /** First byte of the envelope domain separator (`'l'` of
     *  `"lakko/push-routing-secret/v1\x00"`). The cheap pre-filter;
     *  the FFI `decodePushRoutingSecret` is the authoritative
     *  fail-closed check. */
    private const val ENVELOPE_FIRST_BYTE: Int = 0x6c

    /** Result of a subscribe attempt. */
    public enum class SubscribeOutcome { SUBSCRIBED, DEFERRED }

    /** Verdict of the receive-side authority gate ([decideAcceptReceivedSecret]).
     *  [REJECT] means DROP — no persist, no subscribe, no re-broadcast. */
    public enum class ReceiveVerdict { ACCEPT, REJECT }

    /** What [ensure]'s provisioning step must do this call
     *  ([decideEnsureAction]). Subscription is decided separately and
     *  always runs — see [subscribeOrDefer]. */
    public enum class EnsureAction {
        /** Nothing to provision: a non-owner, or an owner whose secret is
         *  both held and confirmed distributed. */
        NONE,

        /** Owner, no secret held: mint + persist (flagged NOT distributed)
         *  + broadcast. */
        MINT_AND_BROADCAST,

        /** Owner, secret held but its distribution was never confirmed:
         *  recompose the envelope from the STORED secret and broadcast it
         *  again. Never re-mints — a second random mint would diverge the
         *  group's `push_gid`. */
        REBROADCAST,
    }

    /**
     * The provisioning half of [ensure] as a PURE function, so the
     * recovery branch is host-JVM unit-testable (the
     * [decideAcceptReceivedSecret] precedent).
     *
     * ## The bug this models away
     *
     * The guard used to be exactly `isOwner && !hasSecret`, with the
     * secret persisted BEFORE the broadcast and the broadcast's failure
     * swallowed. So an owner that minted while offline (or hit a DS
     * error) ended up with the secret durably stored and the guard
     * permanently satisfied: on every later launch / chat-open `ensure`
     * short-circuited and the distribution envelope was NEVER re-sent.
     * Every other member deferred forever ([subscribeOrDefer] never falls
     * back to `group_id`), so **the whole group silently lost push, for
     * good** — the only accidental repair being a [redistribute] after an
     * unrelated add-member commit.
     *
     * Persist-before-broadcast is still right (minting twice DIVERGES the
     * group's `push_gid`; broadcasting twice is inert). What was missing
     * is a durable record of whether the broadcast landed —
     * [distributed]. With it, the failure is recoverable on the next
     * `ensure` instead of terminal.
     *
     * A re-broadcast is safe by construction: it re-sends the SAME bytes,
     * and [decideAcceptReceivedSecret] accepts a byte-identical secret
     * from any sender, so it is idempotent for every member regardless of
     * rank or of what they already hold.
     *
     * @param isOwner is this device the ADR-0022 group owner? Only the
     *   owner ever mints or broadcasts here.
     * @param hasSecret does this device hold a `push_routing_secret` for
     *   the group?
     * @param distributed has the held secret's distribution envelope been
     *   confirmed on the wire? Meaningless (and ignored) when
     *   [hasSecret] is `false`.
     */
    public fun decideEnsureAction(
        isOwner: Boolean,
        hasSecret: Boolean,
        distributed: Boolean,
    ): EnsureAction = when {
        !isOwner -> EnsureAction.NONE
        !hasSecret -> EnsureAction.MINT_AND_BROADCAST
        !distributed -> EnsureAction.REBROADCAST
        else -> EnsureAction.NONE
    }

    /**
     * Outcome of reading a group's ADR-0022 role table. Three DISTINCT
     * states — conflating them is the bug this models away: "no role
     * table" is a legitimate legacy/open group (accept), while "could
     * not read" is an error that must fail closed.
     */
    public sealed interface OwnerLookup {
        /** The group carries a role table. [owner] is the owner's raw
         *  32-byte Ed25519 signature public key; [admins] is the
         *  ADR-0022 admin set (each entry likewise a raw 32-byte
         *  signature public key). Both are the SAME key shape the live
         *  roster (`Group.members()[].signaturePublicKey`) reports, so
         *  the gate compares like with like. */
        public data class Known(
            val owner: ByteArray,
            val admins: List<ByteArray> = emptyList(),
        ) : OwnerLookup {
            override fun equals(other: Any?): Boolean =
                this === other ||
                    (
                        other is Known &&
                            owner.contentEquals(other.owner) &&
                            admins.size == other.admins.size &&
                            admins.indices.all { admins[it].contentEquals(other.admins[it]) }
                        )

            override fun hashCode(): Int =
                admins.fold(owner.contentHashCode()) { acc, a -> 31 * acc + a.contentHashCode() }
        }

        /** The group carries NO role table — a legacy / open group
         *  (ADR-0022 leaves these ungated). Preserves pre-gate
         *  behaviour: accept. */
        public data object NoRoleTable : OwnerLookup

        /** The role table or the member roster could not be read (an
         *  FFI throw). Fail closed. */
        public data object Unavailable : OwnerLookup
    }

    /**
     * The receive-side authority gate — PURE, so the accept/reject matrix
     * is host-JVM unit-testable without the Android framework or the
     * native `.so` (the [me.lakko.app.push.resolveTapGroupHex] /
     * `CommitOutbox.reconcileVerdict` precedent).
     *
     * ## Why a gate at all
     *
     * The authority check used to exist ONLY on the SEND side
     * (`ChatRepository.ensurePushRoutingBestEffort` gates the mint on
     * `ownRole == OWNER`). On RECEIVE every member persisted whatever
     * secret arrived and re-subscribed — so a single plain member
     * could unilaterally rotate the group's `push_gid`: every member
     * overwrites its secret and re-subscribes, and since per-group
     * unsubscribe is never called, each churn strands an orphan push-gw
     * subscription that keeps receiving wakes.
     *
     * ## The rule — OWNER outranks ADMIN outranks MEMBER
     *
     * Authority is expressed as *who may overwrite what*, deliberately
     * WITHOUT persisting an authority level alongside the secret (the
     * stored state stays exactly the 32 secret bytes):
     *
     * | stored | sender | verdict |
     * |---|---|---|
     * | identical to incoming | anyone (member too) | ACCEPT |
     * | any (or none) | OWNER | ACCEPT |
     * | **none** (first-ever) | ADMIN | ACCEPT |
     * | present + changed | ADMIN | REJECT |
     * | any changed | plain member | REJECT |
     * | changed, no role table | any | ACCEPT |
     * | changed, roles unreadable / sender unresolved | any | REJECT |
     *
     *   - **Identical → always ACCEPT**, regardless of sender: it needs
     *     no authority because it rotates nothing. This is what keeps the
     *     post-add-member [redistribute] idempotent for every existing
     *     member.
     *   - **The OWNER may always rotate** — the "owner takes priority"
     *     half. Because owner-acceptance does not depend on what is
     *     stored, the owner wins regardless of ARRIVAL ORDER: an
     *     owner-minted secret overwrites an admin-seeded one whether it
     *     lands before or after it.
     *   - **An ADMIN may SEED but never OVERWRITE.** ADR-0022 permits
     *     owner *or* admin to add members, and [redistribute] fires after
     *     any add-member commit — so a member added by a non-owner admin
     *     would otherwise hold no secret forever (the owner only
     *     broadcasts at mint time) and never get push routing. Letting an
     *     admin FILL A HOLE closes that delivery gap; refusing an admin
     *     overwrite keeps the rotation-churn attack surface at the owner
     *     alone.
     *   - [OwnerLookup.NoRoleTable] → accept (legacy / open group —
     *     unchanged pre-gate behaviour; such a group has no roles to
     *     check against).
     *   - [OwnerLookup.Unavailable] → REJECT a changed secret (fail
     *     closed, invariant #4): an unreadable roster / role table must
     *     never widen who may rotate.
     *   - A `null` [senderSignatureKey] (an external sender, a leaf
     *     absent from the roster, or a lookup throw) matches neither the
     *     owner key nor any admin key, so a changed secret from one is
     *     refused.
     *
     * Note the sender identity is the MLS-authenticated
     * `ProcessedMessage.Application.senderLeaf` resolved against the
     * live roster — NOT anything carried in the attacker-chosen
     * envelope body.
     *
     * @param senderSignatureKey the sender's 32-byte Ed25519 signature
     *   public key, or `null` when it could not be resolved.
     * @param owner the group's role-table lookup outcome (owner + admins).
     * @param incomingSecret the secret carried by the received envelope.
     * @param storedSecret the secret this device already holds, or
     *   `null` when it holds none.
     */
    public fun decideAcceptReceivedSecret(
        senderSignatureKey: ByteArray?,
        owner: OwnerLookup,
        incomingSecret: ByteArray,
        storedSecret: ByteArray?,
    ): ReceiveVerdict {
        // Unchanged secret -> always idempotent-accept, regardless of
        // sender: it rotates nothing, so a relay by any member is inert
        // (and this is exactly the post-add-member `redistribute`).
        if (storedSecret != null && incomingSecret.contentEquals(storedSecret)) {
            return ReceiveVerdict.ACCEPT
        }
        // The secret CHANGES (or is the first-ever) from here on.
        return when (owner) {
            is OwnerLookup.Unavailable -> ReceiveVerdict.REJECT
            is OwnerLookup.NoRoleTable -> ReceiveVerdict.ACCEPT
            is OwnerLookup.Known -> when {
                senderSignatureKey == null -> ReceiveVerdict.REJECT
                // The owner outranks everyone: may seed AND may rotate,
                // whatever is stored and whatever arrived first.
                senderSignatureKey.contentEquals(owner.owner) -> ReceiveVerdict.ACCEPT
                // An admin may FILL A HOLE (no secret held) but never
                // overwrite one that is already there.
                owner.admins.any { it.contentEquals(senderSignatureKey) } ->
                    if (storedSecret == null) ReceiveVerdict.ACCEPT else ReceiveVerdict.REJECT
                // A plain member (or a non-member key) may never change it.
                else -> ReceiveVerdict.REJECT
            }
        }
    }

    /** Per-`(account, group)` in-flight mint lock. Keyed so two
     *  accounts (or two groups) never contend on one another. */
    private val inflight = ConcurrentHashMap<String, Mutex>()

    /**
     * Cheap length + first-byte pre-filter mirroring the web
     * `tryDecodeSecretEnvelope` gate. A `true` here still routes the
     * bytes through the authoritative fail-closed FFI
     * `decodePushRoutingSecret` (domain + version + exact-length);
     * a `false` avoids an FFI call on every chat message. Pure —
     * host-testable.
     */
    public fun looksLikeSecretEnvelope(bytes: ByteArray): Boolean =
        bytes.size == ENVELOPE_LEN && (bytes[0].toInt() and 0xFF) == ENVELOPE_FIRST_BYTE

    /**
     * Subscribe this device to push wakes for a group UNDER ITS
     * `push_gid`. A member that does not hold the secret **defers**
     * — it never subscribes under the leaky `group_id`. Subscribe
     * failures are swallowed (best-effort; foreground polling still
     * works).
     */
    public suspend fun subscribeOrDefer(
        groupIdHex: String,
        ports: PushRoutingPorts,
    ): SubscribeOutcome {
        val secret = ports.getSecret(groupIdHex) ?: return SubscribeOutcome.DEFERRED
        val pushGid = ports.derivePushGid(secret)
        try {
            ports.subscribeUnderPushGid(pushGid)
        } catch (_: Throwable) {
            // Best-effort — push is a UX nicety; foreground polling covers it.
        }
        return SubscribeOutcome.SUBSCRIBED
    }

    /**
     * RECEIVE side — a member got the distribution envelope: persist
     * the secret + reverse-index entry (idempotent) and subscribe
     * under the derived `push_gid`. Never mints.
     *
     * UNGATED — callers on the wire path MUST use [receiveGated], which
     * runs the [decideAcceptReceivedSecret] authority check first.
     */
    public suspend fun receive(
        groupIdHex: String,
        secret: ByteArray,
        ports: PushRoutingPorts,
    ) {
        val pushGid = ports.derivePushGid(secret)
        // Persisted as ALREADY distributed: this secret arrived off the
        // group journal, so it is by definition on the wire. A receiver
        // never re-broadcasts — only the owner does ([ensure]).
        ports.persist(groupIdHex, secret, pushGid, distributed = true)
        subscribeOrDefer(groupIdHex, ports)
    }

    /**
     * RECEIVE side WITH the authority gate — the entry point the chat
     * receive loop uses. Reads the currently-held secret, runs
     * [decideAcceptReceivedSecret], and only on
     * [ReceiveVerdict.ACCEPT] does it fall through to [receive]. A
     * [ReceiveVerdict.REJECT] is a pure DROP: no persist, no
     * reverse-index write, no subscribe — so nobody below owner rank can
     * rotate the group's `push_gid` (and cannot strand an orphan push-gw
     * subscription).
     *
     * A throwing `getSecret` degrades to "no stored secret", which must
     * stay STRICTER, never more permissive. Losing the stored value
     * loses the byte-identical fast path (stricter by itself), but for
     * an ADMIN sender "no secret held" is precisely the hole-fill the
     * matrix ACCEPTS — so on a throw the admin set is dropped from the
     * authority before deciding, leaving only the owner (who may rotate
     * whatever is stored) able to accept. An unreadable store therefore
     * cannot widen who may write, exactly as an unreadable role table
     * cannot ([OwnerLookup.Unavailable]).
     *
     * @return the verdict, so the caller can log a refusal.
     */
    public suspend fun receiveGated(
        groupIdHex: String,
        secret: ByteArray,
        senderSignatureKey: ByteArray?,
        owner: OwnerLookup,
        ports: PushRoutingPorts,
    ): ReceiveVerdict {
        val read: Result<ByteArray?> = runCatching { ports.getSecret(groupIdHex) }
        val stored = read.getOrNull()
        // On an unreadable store the stored state is UNKNOWN, so strip the
        // admin set: a `storedSecret = null` must not be mistaken for a
        // real hole an admin may fill. The owner is unaffected (it may
        // rotate whatever is stored), so this can only narrow the gate.
        val authority = if (read.isSuccess || owner !is OwnerLookup.Known) {
            owner
        } else {
            OwnerLookup.Known(owner.owner)
        }
        val verdict = decideAcceptReceivedSecret(
            senderSignatureKey = senderSignatureKey,
            owner = authority,
            incomingSecret = secret,
            storedSecret = stored,
        )
        if (verdict == ReceiveVerdict.ACCEPT) {
            receive(groupIdHex, secret, ports)
        }
        return verdict
    }

    /**
     * Ensure a group's push routing is provisioned for this device.
     *
     *   - ADR-0022 OWNER holding no secret yet: mint (32 CSPRNG bytes)
     *     + persist (flagged NOT distributed) + broadcast the
     *     distribution envelope, flagging it distributed ON SUCCESS.
     *   - OWNER holding a secret whose broadcast was never confirmed:
     *     **re-broadcast** it — the envelope is recomposed from the
     *     STORED secret, so nothing is re-minted. This is the recovery
     *     path for a mint whose broadcast failed (offline / DS error);
     *     without it that group never learns its secret and loses push
     *     permanently. See [decideEnsureAction].
     *   - Then subscribe under `push_gid` if the secret is held
     *     (owner just minted it; a member may already hold one from a
     *     prior receive), else defer.
     *
     * The single funnel for BOTH the create / join path AND the
     * bootstrap legacy-sweep. Non-owners never mint NOR broadcast here.
     * A per-`(account, group)` in-flight lock coalesces concurrent
     * callers so the random secret is minted exactly once.
     */
    public suspend fun ensure(
        accountIdB64: String,
        groupIdHex: String,
        isOwner: Boolean,
        ports: PushRoutingPorts,
    ) {
        val mutex = inflight.getOrPut(lockKey(accountIdB64, groupIdHex)) { Mutex() }
        mutex.withLock {
            // A non-owner neither mints nor broadcasts, so its store is
            // not even read here.
            val action = if (!isOwner) {
                EnsureAction.NONE
            } else {
                val hasSecret = ports.hasSecret(groupIdHex)
                // An unreadable flag is treated as NOT distributed: a
                // needless re-broadcast is inert (byte-identical, so
                // every member idempotently accepts it), whereas
                // guessing "distributed" would resurrect the
                // permanent-no-push bug.
                val distributed = !hasSecret ||
                    runCatching { ports.isDistributed(groupIdHex) }.getOrDefault(false)
                decideEnsureAction(isOwner = true, hasSecret = hasSecret, distributed = distributed)
            }

            when (action) {
                EnsureAction.MINT_AND_BROADCAST -> {
                    val secret = ports.mintSecret()
                    val pushGid = ports.derivePushGid(secret)
                    // Persist BEFORE broadcasting, flagged NOT distributed.
                    // The order is deliberate: minting twice would diverge
                    // the group's push_gid, so a crash between the two must
                    // leave the secret durable — and the flag is what makes
                    // the un-broadcast state recoverable rather than
                    // terminal (the next `ensure` takes the REBROADCAST
                    // branch below).
                    ports.persist(groupIdHex, secret, pushGid, distributed = false)
                    broadcastAndMark(groupIdHex, secret, ports)
                }

                EnsureAction.REBROADCAST -> {
                    // Recomposed from the stored secret — never re-minted.
                    val secret = ports.getSecret(groupIdHex)
                    if (secret != null) broadcastAndMark(groupIdHex, secret, ports)
                }

                EnsureAction.NONE -> Unit
            }

            // Subscribe (or defer) under push_gid. Owner just minted ->
            // subscribes; a member with no secret -> deferred (never
            // under group_id).
            subscribeOrDefer(groupIdHex, ports)
        }
    }

    /**
     * Broadcast the distribution envelope for [secret] and, ONLY on
     * success, durably flag the group's row distributed.
     *
     * Best-effort by design: a failure is swallowed and the row stays
     * flagged NOT distributed, so the next [ensure] re-broadcasts. A
     * throwing `markDistributed` lands in the same place — the row
     * stays pending and the (idempotent) broadcast simply repeats.
     */
    private suspend fun broadcastAndMark(
        groupIdHex: String,
        secret: ByteArray,
        ports: PushRoutingPorts,
    ) {
        try {
            ports.broadcast(ports.composeEnvelope(secret))
            ports.markDistributed(groupIdHex)
        } catch (_: Throwable) {
            // Left pending on purpose — the next `ensure` retries.
        }
    }

    /**
     * Re-broadcast the CURRENT secret to a group (used right after an
     * add-member commit so the new member learns it on their next
     * poll). No-op when this device holds no secret. Idempotent for
     * existing members (a re-received secret is the same value → an
     * idempotent re-persist). Best-effort.
     *
     * Marks the row distributed on success (it is a confirmed send of
     * exactly the envelope [ensure] would re-send), so a mint whose own
     * broadcast failed is also healed by this path. A throw propagates
     * to the caller and leaves the row pending.
     */
    public suspend fun redistribute(
        groupIdHex: String,
        ports: PushRoutingPorts,
    ) {
        val secret = ports.getSecret(groupIdHex) ?: return
        ports.broadcast(ports.composeEnvelope(secret))
        ports.markDistributed(groupIdHex)
    }

    private fun lockKey(accountIdB64: String, groupIdHex: String): String =
        "$accountIdB64\u0000$groupIdHex"
}

/**
 * The FFI + stores + push-gw + journal ops the [GroupPushRouting]
 * orchestration composes. Faked in host tests (so the load-bearing
 * rules are exercised without the native `.so`);
 * [NativePushRoutingPorts] binds the real dependencies.
 *
 * All methods are bound to a single account — the production impl
 * closes over `accountIdB64`, so the seam keys only by `groupIdHex`.
 */
public interface PushRoutingPorts {
    /** Mint a fresh `push_routing_secret` (32 CSPRNG bytes). */
    public fun mintSecret(): ByteArray

    /** Derive the stable opaque `push_gid` (32 bytes) from a secret.
     *  Deterministic across every member that holds the same secret. */
    public fun derivePushGid(secret: ByteArray): ByteArray

    /** Compose the E2EE distribution-envelope bytes for a secret. */
    public fun composeEnvelope(secret: ByteArray): ByteArray

    /** Has this device a persisted secret for `groupIdHex`? */
    public fun hasSecret(groupIdHex: String): Boolean

    /** Read the persisted secret for `groupIdHex`, or `null`. */
    public fun getSecret(groupIdHex: String): ByteArray?

    /** Has the held secret's distribution envelope been confirmed on
     *  the wire? `true` when nothing is held (there is nothing pending
     *  to re-send) and for a LEGACY row written before the flag
     *  existed. Drives [GroupPushRouting.EnsureAction.REBROADCAST]. */
    public fun isDistributed(groupIdHex: String): Boolean

    /** Persist the secret (encrypted at rest) AND the
     *  `push_gid -> (account, group)` reverse-index entry, together
     *  with whether its distribution envelope is already on the wire
     *  ([distributed]). The owner mint passes `false`; a received
     *  secret passes `true`. */
    public fun persist(
        groupIdHex: String,
        secret: ByteArray,
        pushGid: ByteArray,
        distributed: Boolean,
    )

    /** Durably flag `groupIdHex`'s row distributed — called ONLY after
     *  a broadcast actually succeeded. No-op when no row is held or it
     *  is already flagged. */
    public fun markDistributed(groupIdHex: String)

    /** Subscribe this device to push-gw under `pushGid`. */
    public suspend fun subscribeUnderPushGid(pushGid: ByteArray)

    /** Post the distribution-envelope bytes to the group journal as
     *  an E2EE MLS application message (persist-before-publish,
     *  fail-closed). Only invoked on the owner-mint branch. */
    public suspend fun broadcast(envelope: ByteArray)
}

/**
 * Production [PushRoutingPorts] — binds the real
 * [PushRoutingSecretStore] + [PushRouteStore] + UniFFI derivation /
 * compose + push-gw subscribe. `broadcastFn` is supplied by
 * `ChatRepository` (it owns the MLS group + the persist-before-
 * publish control-message path).
 */
public class NativePushRoutingPorts(
    private val context: Context,
    private val accountIdB64: String,
    private val routeStore: PushRouteStore,
    private val subscribeFn: suspend (ByteArray) -> Unit,
    private val broadcastFn: suspend (ByteArray) -> Unit,
) : PushRoutingPorts {

    override fun mintSecret(): ByteArray =
        ByteArray(PushRoutingSecretStore.SECRET_LENGTH).also { SecureRandom().nextBytes(it) }

    override fun derivePushGid(secret: ByteArray): ByteArray = ffiDerivePushGid(secret)

    override fun composeEnvelope(secret: ByteArray): ByteArray = composePushRoutingSecret(secret)

    override fun hasSecret(groupIdHex: String): Boolean =
        PushRoutingSecretStore.has(context, accountIdB64, groupIdHex)

    override fun getSecret(groupIdHex: String): ByteArray? =
        PushRoutingSecretStore.get(context, accountIdB64, groupIdHex)

    override fun isDistributed(groupIdHex: String): Boolean =
        PushRoutingSecretStore.isDistributed(context, accountIdB64, groupIdHex)

    override fun persist(
        groupIdHex: String,
        secret: ByteArray,
        pushGid: ByteArray,
        distributed: Boolean,
    ) {
        PushRoutingSecretStore.put(context, accountIdB64, groupIdHex, secret, distributed)
        routeStore.put(PushClient.pseudonymHex(pushGid), accountIdB64, groupIdHex)
    }

    override fun markDistributed(groupIdHex: String) {
        PushRoutingSecretStore.markDistributed(context, accountIdB64, groupIdHex)
    }

    override suspend fun subscribeUnderPushGid(pushGid: ByteArray) {
        subscribeFn(pushGid)
    }

    override suspend fun broadcast(envelope: ByteArray) {
        broadcastFn(envelope)
    }
}
