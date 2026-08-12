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
 * [decideEnsureAction]. That flag is per-GROUP, so the post-add-member
 * [redistribute] rewinds it on a failure too: otherwise a group whose
 * mint DID land stays flagged distributed and the newly-added member
 * — the only one missing the secret — defers push permanently.
 *
 * Note the RETRY is not owner-gated, only the MINT is. ADR-0022 lets an
 * ADMIN add members, so the device whose [redistribute] failed — and
 * whose row therefore holds the only record of it — is frequently not
 * the owner; an owner-only retry would leave that flag inert and the
 * new member unreachable. Re-sending a secret the device ALREADY HOLDS
 * requires no authority, because the receive gate accepts byte-identical
 * bytes from any sender before rank is consulted. Only the mint needs
 * rank, since two independent mints of 32 random bytes would diverge the
 * group's `push_gid`.
 *
 * On the RECEIVE side that mint authority is mirrored by a rank gate
 * ([decideAcceptReceivedSecret]): the owner may always rotate, an
 * ADR-0022 admin may SEED a device that holds nothing (so a member added
 * by a non-owner admin still gets routing) AND rotate an established
 * secret (ADR-0043 option (b) — an admin may commit the REMOVE that makes
 * a rotation necessary), and a plain member may only relay a
 * byte-identical secret.
 *
 * ## ADR-0043 — the routing id ROTATES when a member is removed
 *
 * The id is stable across every ordinary commit, but a REMOVAL re-keys
 * it ([rotate] / [rotateIfMembershipShrank]): otherwise the evicted
 * member keeps deriving the current `push_gid` and push-gw keeps waking
 * it, so it learns "this group was active at time T" indefinitely. The
 * rotation is paired with an unsubscribe of the superseded id on EVERY
 * member ([retireOldPushGid]) — without it each rotation would leave one
 * dead push-gw row per remaining device, turning a live cluster into a
 * versioned archive of clusters, which is strictly worse than not
 * rotating at all.
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
        /** Nothing to provision: a holder whose secret is confirmed
         *  distributed, or a NON-owner holding nothing at all (it must
         *  wait for the envelope — it may never mint). */
        NONE,

        /** Owner, no secret held: mint + persist (flagged NOT distributed)
         *  + broadcast. The one branch that still requires owner rank. */
        MINT_AND_BROADCAST,

        /** ANY holder — owner or not — whose secret's distribution was
         *  never confirmed: recompose the envelope from the STORED secret
         *  and broadcast it again. Never re-mints — a second random mint
         *  would diverge the group's `push_gid`. */
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
     * ## Why the retry branch is NOT owner-gated
     *
     * The retry used to be, and that left the second half of the same hole
     * open. ADR-0022 lets an ADMIN add members, and [redistribute] fires
     * after ANY add-member commit — so: an admin adds a member, the
     * admin's re-send fails, the admin's row rewinds to pending, and with
     * an owner-only retry that pending flag is INERT. The owner's own row
     * is still flagged distributed and it never learns anything went
     * wrong, so nothing ever re-sends and the newly-added member defers
     * push forever. The rewind would fix nothing on the one device that
     * actually observed the failure.
     *
     * Re-broadcasting needs no authority, and that is not a concession —
     * it falls straight out of the receive gate. [decideAcceptReceivedSecret]
     * accepts a byte-identical secret from ANY sender, checked BEFORE any
     * authority is consulted, precisely because identical bytes rotate
     * nothing. So [REBROADCAST] — which re-sends the exact stored bytes,
     * never re-composed from anything new — is accepted by every peer
     * whatever the sender's rank, and grants no capability a member did
     * not already have (it could always replay an identical envelope off
     * the journal and be accepted).
     *
     * Owner rank is required for the MINT alone, and for a hard reason:
     * the secret is 32 RANDOM bytes, so two independent mints DIVERGE the
     * group's `push_gid` and break routing for everyone. Re-sending a
     * secret the device already holds cannot diverge anything. Hence the
     * split below: [MINT_AND_BROADCAST] keeps its owner gate, [REBROADCAST]
     * is open to any holder, and a non-owner holding NOTHING still does
     * nothing at all (it may never mint — it waits for the envelope).
     *
     * @param isOwner is this device the ADR-0022 group owner? Gates the
     *   MINT only — a non-owner may still re-send a secret it holds.
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
        // Holding nothing: only the owner may mint. Everyone else waits
        // for the distribution envelope — never mints, never broadcasts.
        !hasSecret -> if (isOwner) EnsureAction.MINT_AND_BROADCAST else EnsureAction.NONE
        // Holding a secret whose send was never confirmed: re-send it,
        // whatever this device's rank. Identical bytes need no authority.
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
     * | **none** (first-ever) | ADMIN | ACCEPT (seed) |
     * | present + changed | ADMIN | ACCEPT (rotate, ADR-0043 (b)) |
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
     *     lands before or after it. That branch is still evaluated FIRST,
     *     so widening the admin tier below did not disturb it.
     *   - **An ADMIN may SEED *and* OVERWRITE** (ADR-0043, ratified
     *     option (b)). Two distinct things depend on it. SEEDING a hole
     *     is what makes an admin-performed ADD work: ADR-0022 permits
     *     owner *or* admin to add members and [redistribute] fires after
     *     any add-member commit, so a member added by a non-owner admin
     *     would otherwise hold no secret forever (the owner only
     *     broadcasts at mint time) and never get push routing.
     *     OVERWRITING an established secret is what makes ADR-0043
     *     rotation-on-removal reachable at all, since ADR-0022 also lets
     *     an admin commit the REMOVE — refusing the overwrite left an
     *     admin able to evict a member but unable to retire that member's
     *     routing id, so the eviction completed with the ex-member's wake
     *     stream intact.
     *
     *     The cost is recorded honestly in ADR-0043 § fork (b): it widens
     *     G-36 residual 3 from "a malicious admin can seed a secret to a
     *     member who holds none" to "a malicious admin can churn the
     *     group's routing id at will". The capability gain is small (an
     *     admin can already evict members outright) and the churn is a
     *     wake OUTAGE, never a downgrade — a member without the current
     *     secret still DEFERS, it never falls back to the leaky
     *     `group_id`.
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
                // An admin may FILL A HOLE *and* OVERWRITE an established
                // secret (ADR-0043 ratified option (b)) — ADR-0022 lets an
                // admin commit both the ADD that needs a seed and the REMOVE
                // that needs a rotation. What is stored no longer gates the
                // admin's authority.
                owner.admins.any { it.contentEquals(senderSignatureKey) } -> ReceiveVerdict.ACCEPT
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
        // An UNREADABLE store defers exactly as an absent secret does. It
        // must not throw: this is the tail of every receive / ensure /
        // rotate path, and an escaping read error there would skip the
        // ADR-0043 teardown that follows it. Deferring is the safe verdict
        // either way — it never falls back to the leaky `group_id`.
        val secret = runCatching { ports.getSecret(groupIdHex) }.getOrNull()
            ?: return SubscribeOutcome.DEFERRED
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
        // group journal, so its distribution envelope is provably on the
        // wire. Since [decideEnsureAction]'s REBROADCAST branch is open to
        // ANY holder, this flag is what keeps a receiver from re-sending an
        // envelope it just watched travel: a settled row is never retried
        // by anyone.
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
     * A throwing `getSecret` degrades to "no stored secret", which stays
     * STRICTER, never more permissive: it loses the byte-identical fast
     * path, so an unreadable store can only push a decision INTO the
     * authority check, never around it.
     *
     * That throw used to additionally strip the admin set from the
     * authority, because under the ADR-0042 rule "no secret held" was
     * precisely the hole an admin was allowed to fill, and an unknown
     * stored state must not be mistaken for a real hole. ADR-0043 option
     * (b) removed the premise: an admin may now write whatever is stored,
     * so the stored state no longer gates an admin's authority at all and
     * the narrowing became vestigial. Keeping it would REFUSE the very
     * admin rotation option (b) exists to permit, on nothing more than a
     * transient store error. An unreadable ROLE TABLE still fails closed
     * ([OwnerLookup.Unavailable]) — that is the check that guards who may
     * write.
     *
     * ADR-0043 — when an accepted secret CHANGES the one this device held,
     * the superseded `push_gid` is retired here too ([retireOldPushGid]):
     * every member tears down its OWN push-gw row, because
     * `POST /push/group/unsubscribe` deletes exactly the `(device, group)`
     * row named in the request and a client knows only its own device
     * pseudonym — no one can delete anyone else's. Skipping it on the
     * non-rotating members is what would turn each rotation into a live
     * cluster PLUS a frozen snapshot of the previous one (ADR-0043
     * § Context calls that strictly worse than not rotating at all). The
     * old secret is read from the snapshot taken BEFORE [receive] persists,
     * since the row is overwritten in place.
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
        val verdict = decideAcceptReceivedSecret(
            senderSignatureKey = senderSignatureKey,
            owner = owner,
            incomingSecret = secret,
            storedSecret = stored,
        )
        if (verdict == ReceiveVerdict.ACCEPT) {
            try {
                receive(groupIdHex, secret, ports)
                // ADR-0043 — the applied secret SUPERSEDED one this device
                // held, so the id it was subscribed under is now dead. Retire
                // it. Read from the pre-persist snapshot above: `receive`
                // overwrote the row in place, so after it the old bytes are
                // unrecoverable and the retiring id would be unnameable.
                if (stored != null && !stored.contentEquals(secret)) {
                    retireOldPushGid(stored, ports)
                }
            } catch (_: Throwable) {
                // Best-effort — the poll loop calls this per message, so a
                // store failure must never escape it. The verdict is still
                // reported so the caller logs the same thing either way.
            }
        }
        return verdict
    }

    /**
     * Ensure a group's push routing is provisioned for this device.
     *
     *   - ADR-0022 OWNER holding no secret yet: mint (32 CSPRNG bytes)
     *     + persist (flagged NOT distributed) + broadcast the
     *     distribution envelope, flagging it distributed ON SUCCESS.
     *     Minting is the ONLY owner-gated branch.
     *   - ANY holder — owner or not — whose secret's broadcast was never
     *     confirmed: **re-broadcast** it — the envelope is recomposed
     *     from the STORED secret, so nothing is re-minted. This is the
     *     recovery path both for a mint whose broadcast failed and for a
     *     failed post-add-member [redistribute] on an ADMIN's device
     *     (ADR-0022 lets an admin add members); without it the group —
     *     or, in the admin case, just the newly-added member — never
     *     learns the secret and loses push permanently. Re-sending
     *     identical bytes needs no authority, so it is safe on any
     *     device. See [decideEnsureAction].
     *   - A NON-owner holding NOTHING does nothing: it never mints, and
     *     simply waits for the distribution envelope.
     *   - Then subscribe under `push_gid` if the secret is held
     *     (owner just minted it; a member may already hold one from a
     *     prior receive), else defer.
     *
     * The single funnel for BOTH the create / join path AND the
     * bootstrap legacy-sweep. A per-`(account, group)` in-flight lock
     * coalesces concurrent callers so the random secret is minted
     * exactly once.
     */
    public suspend fun ensure(
        accountIdB64: String,
        groupIdHex: String,
        isOwner: Boolean,
        ports: PushRoutingPorts,
    ) {
        val mutex = inflight.getOrPut(lockKey(accountIdB64, groupIdHex)) { Mutex() }
        mutex.withLock {
            // The rank check no longer short-circuits ahead of the store:
            // a NON-owner can hold a pending row (its own post-add-member
            // `redistribute` failed and rewound it), and that row is only
            // ever re-sent if it is actually looked at. `isOwner` is now
            // just one input to the decision.
            val hasSecret = ports.hasSecret(groupIdHex)
            // The flag read is still skipped when nothing is held — which
            // is exactly the common non-owner NONE case (a member awaiting
            // the envelope), so the cheap membership check remains the
            // only store touch on that path.
            //
            // An unreadable flag is treated as NOT distributed: a needless
            // re-broadcast is inert (byte-identical, so every member
            // idempotently accepts it), whereas guessing "distributed"
            // would resurrect the permanent-no-push bug.
            val distributed = !hasSecret ||
                runCatching { ports.isDistributed(groupIdHex) }.getOrDefault(false)
            val action = decideEnsureAction(
                isOwner = isOwner,
                hasSecret = hasSecret,
                distributed = distributed,
            )

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
     * broadcast failed is also healed by this path.
     *
     * ## Why a FAILURE marks the row PENDING
     *
     * The `distributed` flag is per-GROUP, not per-member, so on its own
     * it heals only a failed MINT broadcast. The uncovered case: the mint
     * landed (`distributed = true`), a member was then added, and THIS
     * re-send failed (offline / DS error). The flag is already `true`, so
     * [decideEnsureAction] returns [EnsureAction.NONE] and no sweep ever
     * re-sends — the newly-added member never learns the secret and
     * defers push forever ([subscribeOrDefer] never falls back to
     * `group_id`). That is the same permanent-no-push bug the flag was
     * introduced to kill, merely scoped to one member instead of the
     * whole group.
     *
     * Flipping the row back to pending on a failure hands it to the
     * existing [EnsureAction.REBROADCAST] branch, which retries on every
     * later launch / chat-open until one send lands. The retry is safe
     * and cheap by construction: it re-sends the SAME bytes, which
     * [decideAcceptReceivedSecret] accepts as byte-identical from ANY
     * sender, so it is inert for every existing member and is exactly
     * what the new member needs. The cost ceiling is one extra control
     * message per failed redistribute.
     *
     * A failing [markPending] is swallowed — the original broadcast
     * failure is what the caller must see, and re-flagging is a
     * best-effort repair, not a correctness gate.
     *
     * The rewind lands on the device that actually observed the failure,
     * and that device is often NOT the owner: ADR-0022 lets an ADMIN add
     * members, so an admin reaches this path too. That is exactly why
     * [decideEnsureAction] arms [EnsureAction.REBROADCAST] for ANY holder
     * rather than the owner alone — an owner-gated retry would leave the
     * admin's pending flag inert while the owner's row (still flagged
     * distributed) gave it no reason to act, and the newly-added member
     * would never learn the secret. Re-sending the identical stored bytes
     * needs no authority (the receive gate accepts them from any sender
     * before rank is consulted), so the retry is safe wherever it fires.
     *
     * A throw propagates to the caller (which is best-effort).
     */
    public suspend fun redistribute(
        groupIdHex: String,
        ports: PushRoutingPorts,
    ) {
        val secret = ports.getSecret(groupIdHex) ?: return
        try {
            ports.broadcast(ports.composeEnvelope(secret))
        } catch (e: Throwable) {
            runCatching { ports.markPending(groupIdHex) }
            throw e
        }
        ports.markDistributed(groupIdHex)
    }

    // -----------------------------------------------------------------
    // ADR-0043 — rotate the routing secret when a member is REMOVED
    // -----------------------------------------------------------------
    //
    // ## What rotation buys, stated precisely
    //
    // A removed member keeps the group's `push_routing_secret` on disk, so
    // it can still derive the current `push_gid` — and since nothing tears
    // its subscription down, push-gw keeps waking that device for a group
    // it was evicted from. The leak is not storage hygiene: an ex-member
    // learns "this group was active at time T" on every send, indefinitely.
    //
    // It cannot be closed by unsubscribing, because
    // `POST /push/group/unsubscribe` deletes exactly the `(device, group)`
    // row named in the request and a client knows only its OWN device
    // pseudonym — no remaining member can reach the ex-member's row, and
    // the ex-member's client has no "you were removed" handling to do it
    // itself. Re-keying is the only mechanism the GROUP has: after a
    // rotation nobody ever notifies the old id again, so that row goes
    // INERT without the ex-member's cooperation.
    //
    // What it does NOT buy: unlinkability of the CLUSTER. The provider sees
    // `{A,B,C,D}` under the old id and `{A,B,C}` under the new one and
    // re-links by token-set intersection — the set difference even names
    // the evicted device. A deliberate, recorded trade (ADR-0043 § crux);
    // closing it needs per-device-blinded ids (G-36 residual 1).
    //
    // ## Rotate on REMOVAL only, never on add
    //
    // An added member is *supposed* to route — [redistribute] already hands
    // it the current secret. Rotating on add would also turn that
    // redistribute into a CHANGED secret against every existing member's
    // stored one, on every add, for zero privacy gain — and it would break
    // the admin-seed delivery path outright.

    /** The local device's ADR-0022 role, in the shape the pure rotation
     *  core consumes. [NONE] is an open / legacy group with no role table
     *  (the `null` of `ChatRepository.ownRole`). Deliberately NOT the
     *  UniFFI `Role` enum, so the decision core stays free of FFI types and
     *  host-JVM testable. */
    public enum class RotationRole { OWNER, ADMIN, MEMBER, NONE }

    /** Verdict of the pure [membershipRotationDecision] core. */
    public enum class RotationDecision {
        /** Membership SHRANK and this device may rotate. */
        ROTATE,

        /** Membership grew or is unchanged — an add / role change /
         *  ordinary commit. The routing id stays stable, which is the whole
         *  point of rotating only at removals. */
        SKIP_NO_SHRINK,

        /** No usable before/after snapshot (a first observation of the
         *  group, or a `members()` read that threw). NEVER rotate on an
         *  unknown delta — a missing snapshot would otherwise read as
         *  "everyone departed". */
        SKIP_UNKNOWN_MEMBERSHIP,

        /** A shrink, but this device is a plain member (or the group has no
         *  role table). Rotating is owner-or-admin only. */
        SKIP_NOT_AUTHORIZED,
    }

    /** Outcome of the rotation orchestration — the decision values plus
     *  what the IO half resolved to. */
    public enum class RotationOutcome {
        /** Minted, persisted, broadcast, re-subscribed, old id retired. */
        ROTATED,

        /** Membership grew or is unchanged. */
        SKIP_NO_SHRINK,

        /** No usable before/after snapshot. */
        SKIP_UNKNOWN_MEMBERSHIP,

        /** Neither owner nor admin (or no role table). */
        SKIP_NOT_AUTHORIZED,

        /** This device holds no secret, so there is nothing to ROTATE.
         *  Minting is owner-only and lives in [ensure]; rotation
         *  deliberately never mints a first secret, so an admin can never
         *  become a second minter and diverge the group's `push_gid`.
         *  Also covers an UNREADABLE row — one whose old id could never be
         *  named, so rotating it would strand it subscribed forever. */
        SKIP_NO_SECRET,
    }

    /**
     * ADR-0043 — retire a SUPERSEDED `push_gid`: drop this device's push-gw
     * subscription under it, then delete its reverse-index row.
     *
     * Called on BOTH sides of a rotation with the OLD secret — by the
     * rotator after it re-arms under the new id ([rotate]), and by every
     * remaining member when [receiveGated] applies a CHANGED secret. Both
     * must, and for the same reason: unsubscribe deletes exactly the
     * `(device, group)` row named in the request and a client knows only
     * its OWN device pseudonym, so nobody can tear down anyone else's row.
     * If only the rotator unsubscribed, every OTHER member would leave a
     * live row under the old id — one dead subscription per member per
     * rotation, the "versioned cluster archive" ADR-0043 § Context calls
     * strictly worse than not rotating at all.
     *
     * The SUBSCRIPTION teardown is what carries the privacy value; the
     * route row is local bookkeeping (a reverse-map miss already degrades
     * to the generic "New message" banner under the ADR-0041 fail-closed
     * posture), so it is deleted immediately — the simpler invariant —
     * rather than retained for an overlap window.
     *
     * Best-effort and never throws: the caller has ALREADY switched to the
     * new id, so a failure here must not unwind the rotation. The cost of a
     * failed unsubscribe is a stale row that nothing notifies again.
     */
    public suspend fun retireOldPushGid(oldSecret: ByteArray, ports: PushRoutingPorts) {
        val oldGid = try {
            ports.derivePushGid(oldSecret)
        } catch (_: Throwable) {
            return
        }
        try {
            ports.unsubscribeFromPushGid(oldGid)
        } catch (_: Throwable) {
            // The new id is already armed; a failed unsubscribe leaves a row
            // nothing notifies again, not a broken group.
        }
        try {
            ports.deleteRoute(oldGid)
        } catch (_: Throwable) {
            // Local bookkeeping only — a stale reverse-index entry degrades
            // one notification body, never correctness.
        }
    }

    /**
     * Pure decision core — did the membership SHRINK, and may this device
     * rotate? No IO, so the whole matrix is host-JVM unit-testable (the
     * [decideEnsureAction] / [decideAcceptReceivedSecret] precedent); a
     * verbatim port of the web `membershipRotationDecision`.
     *
     * "Shrank" = at least one member present in the previous snapshot is
     * absent from the current one. Deliberately a SET difference, not a
     * count comparison and not a leaf-index comparison: OpenMLS reuses a
     * vacated leaf for the next Add, so a poll batch carrying a Remove AND
     * an Add leaves both the count and the index set unchanged while a
     * member genuinely was evicted — and that member must still lose its
     * routing id. The identities are the Ed25519 signature public keys,
     * which are stable for a member's lifetime in the group.
     *
     * The authority check is owner-OR-admin (ADR-0043 ratified option (b)),
     * matching who ADR-0022 lets commit the Remove in the first place.
     * [RotationRole.NONE] (no role table) does NOT rotate: such a group has
     * no owner, so nobody ever minted a secret for it and there is nothing
     * to rotate — and letting every member of an un-owned group rotate
     * would have them each mint divergent secrets.
     *
     * @param previousMemberKeys member identities observed BEFORE the
     *   commits landed, or `null` when unknown (a torn `members()` read).
     * @param currentMemberKeys the same, observed AFTER.
     */
    public fun membershipRotationDecision(
        previousMemberKeys: List<String>?,
        currentMemberKeys: List<String>?,
        ownRole: RotationRole,
    ): RotationDecision {
        if (previousMemberKeys == null || currentMemberKeys == null) {
            return RotationDecision.SKIP_UNKNOWN_MEMBERSHIP
        }
        // A first observation has no baseline to diff against.
        if (previousMemberKeys.isEmpty()) return RotationDecision.SKIP_UNKNOWN_MEMBERSHIP
        val present = currentMemberKeys.toHashSet()
        val departed = previousMemberKeys.any { it !in present }
        if (!departed) return RotationDecision.SKIP_NO_SHRINK
        if (ownRole != RotationRole.OWNER && ownRole != RotationRole.ADMIN) {
            return RotationDecision.SKIP_NOT_AUTHORIZED
        }
        return RotationDecision.ROTATE
    }

    /**
     * Per-`(account, group)` ROTATION lock — a **serializing** [Mutex],
     * separate from the [inflight] mint lock. (The web `rotating` promise
     * chain and the iOS `rotating` task chain are the same shape.)
     *
     * Two triggers can observe a removal (this device's own
     * `removeMemberFromGroup` and a concurrent poll-loop diff). Running
     * them concurrently is the damaging case: both would read the SAME old
     * secret, both mint, the second persist wins — leaving the first new
     * `push_gid` subscribed and indexed but never notified again, i.e.
     * exactly the orphan this ADR exists to prevent.
     *
     * **A second trigger therefore WAITS and then rotates again; it is
     * never folded into the rotation already running.** COALESCING is only
     * correct when both triggers observe the SAME removal, and it is
     * silently wrong when they observe DIFFERENT ones: a rotation seals its
     * envelope at the epoch it starts in, then spends three network
     * round-trips (broadcast, subscribe, unsubscribe) in flight. A removal
     * committed inside that window would be folded into a rotation whose
     * envelope the *new* evictee was still a member of — so that evictee
     * opens it, learns the current secret, keeps a live subscription, and
     * nothing ever rotates again (the removing device's own poll baseline
     * is already post-removal, so no shrink is ever seen). The ADR's single
     * load-bearing property would be void for that member.
     *
     * Serializing rules it out: because [rotate] reads the old secret INSIDE
     * `withLock`, the queued second rotation runs in full against the first
     * rotation's output, so every intermediate id is retired in turn, each
     * eviction is followed by a seal at a strictly later epoch, and both
     * callers observe a real rotation.
     */
    private val rotatingLocks = ConcurrentHashMap<String, Mutex>()

    /**
     * ADR-0043 — re-mint the group's `push_routing_secret`, redistribute
     * it, and retire the superseded `push_gid`.
     *
     * **The caller MUST have already published the Remove commit.** The
     * envelope is an MLS application message: sealed at epoch N
     * (pre-removal) the evicted member is still a member of that epoch and
     * can OPEN it, learning the new secret and defeating the whole change.
     * Sealed at N+1 it is unreadable to the removed leaf — RFC 9420 forward
     * secrecy is exactly the property doing the work here. A hard
     * sequencing requirement, not an optimisation. `ChatRepository`
     * enforces it by awaiting the Remove commit's publish before calling.
     *
     * The steps, in the one order that works:
     *
     *   1. read the OLD secret and 2. derive the OLD `push_gid` — **before**
     *      the store write, because the row is keyed `(account, group)` and
     *      is clobbered in place. Persist first and the id being retired can
     *      no longer be NAMED: an undeletable orphan that keeps receiving
     *      wakes, i.e. the exact defect being fixed. (The derivation happens
     *      at teardown time inside [retireOldPushGid]; what is load-bearing
     *      is that the BYTES are captured before the overwrite.)
     *   3. mint the NEW secret and 4. persist it NOT distributed;
     *   5. broadcast, flagging distributed only once the send lands. That
     *      flag is the whole durability mechanism, reused verbatim from the
     *      mint path: a failed rotation broadcast leaves the row PENDING and
     *      [ensure]'s re-broadcast branch — open to ANY holder — re-sends
     *      the stored (i.e. the NEW) secret on the next chat-open / launch.
     *      Without it a failed rotation broadcast strands the whole group:
     *      the rotator alone on the new id, everyone else on the old one,
     *      nobody notifying either;
     *   6. subscribe under the NEW id, then 7./8. retire the OLD one — in
     *      that order, so the device is never unsubscribed from both at once.
     *
     * Rotation NEVER mints a first secret: with none held this is a no-op
     * ([RotationOutcome.SKIP_NO_SECRET]). Minting stays owner-only in
     * [ensure], so widening rotation to admins does not create a second
     * minter.
     *
     * Never throws — push is best-effort and a rotation failure must not
     * unwind the membership change that triggered it.
     */
    public suspend fun rotate(
        accountIdB64: String,
        groupIdHex: String,
        ownRole: RotationRole,
        ports: PushRoutingPorts,
    ): RotationOutcome {
        if (ownRole != RotationRole.OWNER && ownRole != RotationRole.ADMIN) {
            return RotationOutcome.SKIP_NOT_AUTHORIZED
        }
        val mutex = rotatingLocks.getOrPut(lockKey(accountIdB64, groupIdHex)) { Mutex() }
        return mutex.withLock {
            // (1)+(2) — the OLD secret, read BEFORE anything overwrites it.
            // An unreadable row cannot be rotated: the old id could never be
            // named, so a rotation would strand it subscribed forever.
            val oldSecret = runCatching { ports.getSecret(groupIdHex) }.getOrNull()
            if (oldSecret == null) {
                RotationOutcome.SKIP_NO_SECRET
            } else {
                // (3)+(4) — mint + persist PENDING. Persist before
                // broadcasting for the same reason the mint path does: two
                // divergent random secrets are worse than two identical
                // envelopes.
                val next = ports.mintSecret()
                val pushGid = ports.derivePushGid(next)
                ports.persist(groupIdHex, next, pushGid, distributed = false)

                // (5) — redistribute at the POST-removal epoch. A failure
                // leaves the row pending for the existing retry branch; it
                // must NOT abort the local switch, which is already durable.
                broadcastAndMark(groupIdHex, next, ports)

                // (6) — arm the NEW id FIRST, so a failure between the two
                // never leaves this device subscribed to nothing.
                runCatching { subscribeOrDefer(groupIdHex, ports) }

                // (7)+(8) — retire the OLD id: unsubscribe this device's
                // row, drop the stale reverse-index entry.
                retireOldPushGid(oldSecret, ports)
                RotationOutcome.ROTATED
            }
        }
    }

    /**
     * ADR-0043 — the WIRE-OBSERVED trigger: rotate when this device notices
     * that the membership SHRANK, i.e. when SOMEONE ELSE committed the
     * Remove.
     *
     * A Commit surfaces from the poll loop with no membership delta attached
     * (surfacing removed leaves from `lakko-mls` would be exact, but it is
     * an invariant-#3 change to the MLS state machine and needs its own
     * ADR), so the removal is INFERRED from a `members()` snapshot taken
     * either side of the poll batch. That snapshot is already read per poll,
     * so the diff is free.
     *
     * The device that performed the removal itself does NOT come through
     * here: it applied its own commit locally before the poll, so the next
     * poll's baseline is already post-removal and shows no shrink. It
     * rotates directly instead, via [rotate], right after its Remove commit
     * is published.
     *
     * Never throws.
     */
    public suspend fun rotateIfMembershipShrank(
        accountIdB64: String,
        groupIdHex: String,
        ownRole: RotationRole,
        previousMemberKeys: List<String>?,
        currentMemberKeys: List<String>?,
        ports: PushRoutingPorts,
    ): RotationOutcome =
        when (
            membershipRotationDecision(
                previousMemberKeys = previousMemberKeys,
                currentMemberKeys = currentMemberKeys,
                ownRole = ownRole,
            )
        ) {
            RotationDecision.SKIP_NO_SHRINK -> RotationOutcome.SKIP_NO_SHRINK
            RotationDecision.SKIP_UNKNOWN_MEMBERSHIP -> RotationOutcome.SKIP_UNKNOWN_MEMBERSHIP
            RotationDecision.SKIP_NOT_AUTHORIZED -> RotationOutcome.SKIP_NOT_AUTHORIZED
            RotationDecision.ROTATE -> rotate(accountIdB64, groupIdHex, ownRole, ports)
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

    /** Durably flag `groupIdHex`'s row NOT distributed — the inverse of
     *  [markDistributed], called when a broadcast FAILED so THIS device's
     *  next [GroupPushRouting.ensure] re-sends it (the retry is not
     *  owner-gated — see [GroupPushRouting.decideEnsureAction]). No-op
     *  when no row is held or it is already pending. */
    public fun markPending(groupIdHex: String)

    /** Subscribe this device to push-gw under `pushGid`. */
    public suspend fun subscribeUnderPushGid(pushGid: ByteArray)

    /**
     * ADR-0043 — drop this device's push-gw subscription under a
     * SUPERSEDED `pushGid` (`POST /push/group/unsubscribe`, idempotent).
     *
     * The counterpart [subscribeUnderPushGid] never had. It is the only
     * way an evicted member's wake stream can be ended: the endpoint
     * deletes exactly the `(device, group)` row named in the request and a
     * client knows only its OWN device pseudonym, so every member must
     * retire its own row — which is why this is called both by the rotator
     * and by every member whose [GroupPushRouting.receiveGated] applied a
     * CHANGED secret.
     */
    public suspend fun unsubscribeFromPushGid(pushGid: ByteArray)

    /**
     * ADR-0043 — delete the `pushGid -> (account, group)` reverse-index row
     * for a SUPERSEDED id.
     *
     * The stores age asymmetrically: the secret row is keyed
     * `(account, group)` and is OVERWRITTEN in place by a rotation, while
     * the reverse index is keyed by the `push_gid` itself and is only ever
     * ADDED to — so without this every rotation leaks one dead entry.
     */
    public fun deleteRoute(pushGid: ByteArray)

    /** Post the distribution-envelope bytes to the group journal as
     *  an E2EE MLS application message (persist-before-publish,
     *  fail-closed). Invoked on the owner-mint branch and on the
     *  re-send branches ([GroupPushRouting.EnsureAction.REBROADCAST] /
     *  [GroupPushRouting.redistribute]), which any HOLDER may run — the
     *  bytes are identical, so they need no rank. */
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
    private val unsubscribeFn: suspend (ByteArray) -> Unit,
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

    override fun markPending(groupIdHex: String) {
        PushRoutingSecretStore.markPending(context, accountIdB64, groupIdHex)
    }

    override suspend fun subscribeUnderPushGid(pushGid: ByteArray) {
        subscribeFn(pushGid)
    }

    override suspend fun unsubscribeFromPushGid(pushGid: ByteArray) {
        unsubscribeFn(pushGid)
    }

    override fun deleteRoute(pushGid: ByteArray) {
        routeStore.remove(PushClient.pseudonymHex(pushGid))
    }

    override suspend fun broadcast(envelope: ByteArray) {
        broadcastFn(envelope)
    }
}
