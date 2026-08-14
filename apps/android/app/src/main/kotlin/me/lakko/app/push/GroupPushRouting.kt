// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.push

import android.content.Context
import java.security.SecureRandom
import java.util.concurrent.ConcurrentHashMap
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import uniffi.lakko_ffi_uniffi.composePushRoutingSecret
import uniffi.lakko_ffi_uniffi.derivePushGid as ffiDerivePushGid
import uniffi.lakko_ffi_uniffi.pushRoutingSecretIsNewer as ffiPushRoutingSecretIsNewer

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
 * ## ADR-0044 — WHO may write, then WHICH write survives
 *
 * The rank gate above answers only "who may write". Until ADR-0044 that
 * was the WHOLE decision surface, so a **retry** of the current secret
 * and a **replay** of a superseded one were byte-indistinguishable — and
 * that omission made a silent, permanent rotation undo reachable with no
 * malice and no crash (`lakko-security` **F-A**, G-36 residual 9): a
 * device whose broadcast failed holds the PRE-rotation secret in a
 * `pending` row, another admin rotates the group to `S1`, and that
 * device's any-holder re-broadcast re-sends the OLD secret — which every
 * member accepted, retiring `gid(S1)` and migrating the group back onto
 * the routing id the EVICTED member still derives. With no further shrink
 * there is no further rotation, so it was permanent.
 *
 * v2 of the distribution envelope adds the missing recency notion: an
 * 8-byte `mint_epoch` the minter stamps from `Group.epoch()` at MINT
 * time, and the canonical total order on the pair
 * `(mintEpoch, secretBytes)` — defined ONCE, in the audited Rust core,
 * and reached here only through [PushRoutingPorts.isNewer]. Three clients
 * each re-deriving that comparison is precisely the drift class that
 * produced F-A, so no Kotlin re-implementation exists.
 *
 * Two rules carry the fix, and both are easy to break silently:
 *
 *   - **The stamp is minted once and REPLAYED verbatim.** Every
 *     re-broadcast recomposes the envelope from the STORED secret, so it
 *     must replay the epoch that secret was MINTED under. Re-stamping the
 *     current epoch at send time hands the stale secret a NEWER position
 *     than the rotation that superseded it, reproducing F-A with the
 *     ordering gate fully armed. [MintStamp] makes that decision a
 *     TYPE: a caller cannot compose an envelope without naming the
 *     stamp's origin.
 *   - **The future is clamped, BEFORE authority.** An unbounded order
 *     would hand any authorised writer a permanent FREEZE (stamp
 *     `ULong.MAX_VALUE` and nothing can ever exceed it), which is
 *     strictly worse than the churn G-36 residual 3 already records —
 *     a churn self-repairs at the next owner rotation, a freeze does
 *     not.
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

    /** Total distribution-envelope length (bytes) of the ADR-0044 **v2**
     *  envelope: the 29-byte domain separator + 1 version byte + the
     *  8-byte big-endian `mint_epoch` + the 32-byte secret. Mirrors
     *  `lakko_contacts::push_routing` (29 + 1 + 8 + 32). Used only as a
     *  cheap receive-side pre-filter before the authoritative FFI decode.
     *
     *  The v1 envelope was 62 bytes; the flag day is hard (no
     *  dual-accept), and a v1 envelope is refused by the FFI decode with
     *  `UnknownVersion` even if it somehow cleared this pre-filter. Both
     *  cross-version refusals fail closed to DEFER — a member never falls
     *  back to the leaky `group_id` — so invariant #4 holds throughout the
     *  transition. */
    public const val ENVELOPE_LEN: Int = 70

    /** First byte of the envelope domain separator (`'l'` of
     *  `"lakko/push-routing-secret/v1\x00"`). The cheap pre-filter;
     *  the FFI `decodePushRoutingSecret` is the authoritative
     *  fail-closed check. */
    private const val ENVELOPE_FIRST_BYTE: Int = 0x6c

    /** Result of a subscribe attempt. */
    public enum class SubscribeOutcome { SUBSCRIBED, DEFERRED }

    /**
     * Verdict of the receive-side gate ([decideAcceptReceivedSecret]).
     * Every `REJECT_*` means DROP — no persist, no subscribe, no
     * teardown, no re-broadcast.
     *
     * ADR-0044 WIDENED this from a binary `ACCEPT` / `REJECT` for two
     * concrete reasons, neither cosmetic:
     *
     *   1. **The gate's own verdict now decides what gets persisted.**
     *      [ACCEPT_IDENTICAL] must leave the stored `(secret, mintEpoch)`
     *      pair UNTOUCHED (see its own doc), while every other accept
     *      persists the incoming stamp. With a binary verdict the
     *      orchestration would have to re-derive "was it identical?" by
     *      comparing the bytes a SECOND time — a duplicate of rule (1)
     *      free to drift from it, which is the exact defect class
     *      ADR-0044 exists to close.
     *   2. **The two new refusals are operationally distinct.** A
     *      [REJECT_STALE] is the F-A signature (somebody is re-emitting a
     *      superseded secret); a [REJECT_FUTURE] is an out-of-range stamp;
     *      a [REJECT_NOT_AUTHORIZED] is a rank failure. The single
     *      generic log line the caller used to emit would now be actively
     *      wrong for two thirds of the refusals.
     *
     * @property accepts whether the envelope is applied. The partition is
     *   exhaustive: every verdict either persists or drops.
     */
    public enum class ReceiveVerdict(public val accepts: Boolean) {
        /** Byte-identical to the secret already held — an idempotent
         *  no-op re-persist. Needs NO authority and NO ordering, which is
         *  what keeps the any-holder re-broadcast, the post-add
         *  [redistribute] and any relay idempotent for every member
         *  whatever their rank.
         *
         *  Because it clears NEITHER the authority gate NOR the future
         *  clamp, it must never write the INCOMING stamp — doing so would
         *  be an epoch-inflation channel open to a plain member. See
         *  [receiveGated]. */
        ACCEPT_IDENTICAL(true),

        /** A CHANGED secret authored by the ADR-0022 role-table OWNER. */
        ACCEPT_OWNER(true),

        /** A first-ever secret authored by an ADR-0022 ADMIN while this
         *  device PROVABLY holds none — the admin-seed path that makes an
         *  admin-performed add-member deliver routing to the new member. */
        ACCEPT_ADMIN_SEED(true),

        /** A CHANGED secret authored by an ADR-0022 ADMIN over one this
         *  device already holds (or cannot prove it lacks) — ADR-0043
         *  ratified option (b). A DISTINCT verdict from
         *  [ACCEPT_ADMIN_SEED] so a rotation is legible in the log; both
         *  accept. */
        ACCEPT_ADMIN_ROTATE(true),

        /** A CHANGED secret in a group with NO role table (legacy / open
         *  group). No owner or admin set to check against, so this keeps
         *  the pre-gate behaviour rather than silently breaking those
         *  groups. */
        ACCEPT_NO_OWNER(true),

        /** A CHANGED secret from a member that is neither owner nor
         *  admin. */
        REJECT_NOT_AUTHORIZED(false),

        /** A CHANGED secret whose authorship could not be established
         *  (the role table / roster read threw, the frame carried no
         *  sender leaf, or — ADR-0044 — this device's own group epoch
         *  could not be read, so the future clamp had no ceiling to check
         *  against). Dropped, fail-closed. */
        REJECT_UNVERIFIABLE(false),

        /** ADR-0044 — the envelope's `mintEpoch` is ahead of this device's
         *  OWN current group epoch, which an honest stamp can never be
         *  (`stamp <= seal epoch <= receiver's epoch at dispatch`).
         *  Refused BEFORE authority is consulted, so not even the owner
         *  can stamp `ULong.MAX_VALUE` and freeze the group's routing id
         *  permanently. */
        REJECT_FUTURE(false),

        /** ADR-0044 — an AUTHORISED envelope that is not strictly newer
         *  than what this device holds under the canonical
         *  `(mintEpoch, secretBytes)` order. The F-A replay lands here.
         *  Dropped: no persist, no subscribe, and — the one that carries
         *  the security value — no teardown, so a stale replay can never
         *  retire the LIVE routing id. */
        REJECT_STALE(false),
    }

    /**
     * ADR-0044 — an envelope's mint stamp, tagged with WHERE it came
     * from.
     *
     * This type exists for exactly one reason: [PushRoutingPorts.composeEnvelope]
     * takes a `MintStamp`, so a caller **cannot** compose a distribution
     * envelope without stating, in the type, whether the epoch is a FRESH
     * read of `Group.epoch()` at mint time or a REPLAY of the epoch a
     * stored secret was minted under. Getting that wrong is the single
     * mistake that turns the whole ordering gate into a silent no-op, and
     * it is invisible in a `ULong` parameter.
     *
     * Both constructors are `internal` so the only intended producers are
     * the two documented ones:
     *
     *   - [Fresh] — [PushRoutingPorts.freshMintStamp], which reads the
     *     live MLS group epoch. The ONLY way a new epoch value enters the
     *     system.
     *   - [Replayed] — [StoredSecret.replayStamp], which carries the
     *     epoch straight off the stored row.
     */
    public sealed class MintStamp(public val epoch: ULong) {
        /** A FRESH stamp, read from the live `Group.epoch()` at MINT
         *  time. Reached only via [PushRoutingPorts.freshMintStamp]. */
        public class Fresh internal constructor(epoch: ULong) : MintStamp(epoch)

        /** The epoch a STORED secret was minted under, replayed
         *  VERBATIM. Reached only via [StoredSecret.replayStamp]. */
        public class Replayed internal constructor(epoch: ULong) : MintStamp(epoch)
    }

    /**
     * ADR-0044 — a stored `push_routing_secret` together with the epoch
     * it was MINTED under.
     *
     * Read as a PAIR, from ONE store read, deliberately: fetching the
     * secret and its epoch through two separate reads could tear across a
     * concurrent receive (secret from before the write, epoch from
     * after), and a torn pair composes an envelope claiming a live epoch
     * for a superseded secret — precisely the F-A shape the ordering
     * exists to refuse.
     */
    public class StoredSecret(public val secret: ByteArray, public val mintEpoch: ULong) {
        /** The stamp a RE-BROADCAST of [secret] must carry: the epoch it
         *  was minted under, never a fresh one. */
        public val replayStamp: MintStamp.Replayed get() = MintStamp.Replayed(mintEpoch)
    }

    /**
     * ADR-0044 — the canonical recency order: true when `incoming` must
     * supersede `stored`.
     *
     * Ordered on the pair `(mintEpoch, secretBytes)` — epoch first, then
     * the 32 secret bytes lexicographically — and **strictly greater**
     * wins, so equal is NOT newer (which is what keeps a benign retry
     * from reading as an update). Fail-closed to `false` on a
     * wrong-length secret on either side.
     *
     * Declared as a seam so the decision core stays FFI-free and
     * host-testable, and bound in production to the audited Rust
     * `pushRoutingSecretIsNewer` — never re-derived in Kotlin. Three
     * clients each hand-rolling this comparison is the drift class that
     * produced F-A in the first place.
     */
    public fun interface PushRoutingSecretIsNewer {
        /** @see PushRoutingSecretIsNewer */
        public fun isNewer(
            incomingEpoch: ULong,
            incomingSecret: ByteArray,
            storedEpoch: ULong,
            storedSecret: ByteArray,
        ): Boolean
    }

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
     * ## The five steps, in this order (ADR-0044)
     *
     *   1. **identical bytes → [ReceiveVerdict.ACCEPT_IDENTICAL].** No
     *      authority, no ordering. UNCHANGED by ADR-0044, and this is
     *      exactly what preserves retry semantics. Matched on the SECRET
     *      BYTES alone: a retry that reached us over a different route is
     *      still the same value, whatever epoch it claims.
     *   2. **`incomingMintEpoch > ownGroupEpoch` →
     *      [ReceiveVerdict.REJECT_FUTURE].** Evaluated BEFORE authority,
     *      so not even the owner can freeze the group's routing id.
     *      Sound because an honest stamp can never exceed the receiver's
     *      own epoch (`stamp <= seal <= receiver's epoch at dispatch`),
     *      so it never refuses an honest envelope. An UNREADABLE own
     *      epoch is [ReceiveVerdict.REJECT_UNVERIFIABLE] — with no
     *      ceiling a stamp cannot be bounded at all.
     *   3. the ADR-0042 / ADR-0043 **authority** gate below — UNCHANGED,
     *      including the rank, the owner-evaluated-first branch, the
     *      fail-closed refusals, and the legacy no-role-table acceptance.
     *   4. **no stored secret → accept** (the seed path; nothing to order
     *      against).
     *   5. **strictly newer under `(mintEpoch, secretBytes)` → accept**,
     *      otherwise [ReceiveVerdict.REJECT_STALE].
     *
     * ## The rule — OWNER outranks ADMIN outranks MEMBER
     *
     * Authority is expressed as *who may overwrite what*, deliberately
     * WITHOUT persisting an authority level alongside the secret (the
     * stored state stays the 32 secret bytes plus bookkeeping):
     *
     * | stored | sender | verdict |
     * |---|---|---|
     * | identical to incoming | anyone (member too) | ACCEPT_IDENTICAL |
     * | any (or none) | OWNER | ACCEPT_OWNER |
     * | **none** (provably) | ADMIN | ACCEPT_ADMIN_SEED |
     * | present / unprovable | ADMIN | ACCEPT_ADMIN_ROTATE (ADR-0043 (b)) |
     * | any changed | plain member | REJECT_NOT_AUTHORIZED |
     * | changed, no role table | any | ACCEPT_NO_OWNER |
     * | changed, roles unreadable / sender unresolved | any | REJECT_UNVERIFIABLE |
     *
     * ## The semantic narrowing ADR-0044 records
     *
     * A total order and unconditional owner-priority cannot both hold.
     * The owner branch is still decided before anything is read about
     * what is stored, so the owner still clears AUTHORITY first — but its
     * envelope now also has to WIN THE ORDER, and **at an exact epoch tie
     * the bytes decide and the owner can lose to an admin**. That is
     * deliberate: the property owner-priority existed to provide
     * (arrival-order independence) is preserved by a strictly stronger
     * mechanism — a total order is arrival-order independent for EVERY
     * participant, not merely for the owner's envelope. Under ADR-0043
     * option (b) one removal yields N+1 same-epoch mints, so the tie is
     * the common path, not a corner case.
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
     * @param incomingMintEpoch ADR-0044 — the `mint_epoch` that envelope
     *   carried.
     * @param stored what this device already holds — the secret AND the
     *   epoch it was minted under, read as one pair — or `null` when it
     *   holds none (or could not read it, see [storedUnreadable]).
     * @param storedUnreadable set when the local row could NOT be read
     *   (the store threw). `stored == null` then means "unknown", not
     *   "provably absent", so an admin write is reported as a ROTATE
     *   rather than a SEED. It does not change the accept/reject outcome
     *   (ADR-0043 option (b) lets an admin overwrite either way); it
     *   keeps the verdict honest about what was actually observed.
     * @param ownGroupEpoch ADR-0044 — this device's OWN current MLS group
     *   epoch, the clamp ceiling. `null` when `Group.epoch()` could not
     *   be read, which FAILS CLOSED: with no ceiling there is nothing to
     *   bound an incoming stamp against.
     * @param isNewer the canonical order — MUST be
     *   [PushRoutingPorts.isNewer], i.e. the audited Rust
     *   `pushRoutingSecretIsNewer`. Threaded in rather than re-derived
     *   here so the order stays defined once.
     */
    public fun decideAcceptReceivedSecret(
        senderSignatureKey: ByteArray?,
        owner: OwnerLookup,
        incomingSecret: ByteArray,
        incomingMintEpoch: ULong,
        stored: StoredSecret?,
        storedUnreadable: Boolean,
        ownGroupEpoch: ULong?,
        isNewer: PushRoutingSecretIsNewer,
    ): ReceiveVerdict {
        // (1) Unchanged secret -> always idempotent-accept, regardless of
        // sender AND regardless of the stamp it claims: it rotates
        // nothing, so a relay by any member is inert (and this is exactly
        // the post-add-member `redistribute`).
        if (stored != null && incomingSecret.contentEquals(stored.secret)) {
            return ReceiveVerdict.ACCEPT_IDENTICAL
        }
        // The secret CHANGES (or is the first-ever) from here on.
        //
        // (2) ADR-0044 — clamp the future BEFORE authority. An unbounded
        // ordering rule would hand ANY authorised writer a permanent
        // freeze, strictly worse than the churn it already has: a churn
        // self-repairs at the next owner rotation, a freeze does not.
        if (ownGroupEpoch == null) return ReceiveVerdict.REJECT_UNVERIFIABLE
        if (incomingMintEpoch > ownGroupEpoch) return ReceiveVerdict.REJECT_FUTURE
        // (3) Authority — unchanged from ADR-0042 / ADR-0043.
        val authorised = when (owner) {
            is OwnerLookup.Unavailable -> ReceiveVerdict.REJECT_UNVERIFIABLE
            is OwnerLookup.NoRoleTable -> ReceiveVerdict.ACCEPT_NO_OWNER
            is OwnerLookup.Known -> when {
                senderSignatureKey == null -> ReceiveVerdict.REJECT_UNVERIFIABLE
                // The owner outranks everyone on AUTHORITY: it may seed AND
                // rotate, whatever is stored and whatever arrived first.
                senderSignatureKey.contentEquals(owner.owner) -> ReceiveVerdict.ACCEPT_OWNER
                // An admin may FILL A HOLE *and* OVERWRITE an established
                // secret (ADR-0043 ratified option (b)) — ADR-0022 lets an
                // admin commit both the ADD that needs a seed and the REMOVE
                // that needs a rotation. What is stored no longer gates the
                // admin's authority; it only decides which verdict NAMES it.
                owner.admins.any { it.contentEquals(senderSignatureKey) } ->
                    if (stored == null && !storedUnreadable) {
                        ReceiveVerdict.ACCEPT_ADMIN_SEED
                    } else {
                        ReceiveVerdict.ACCEPT_ADMIN_ROTATE
                    }
                // A plain member (or a non-member key) may never change it.
                else -> ReceiveVerdict.REJECT_NOT_AUTHORIZED
            }
        }
        if (!authorised.accepts) return authorised
        // (4) Nothing stored to order against — the seed path.
        if (stored == null) return authorised
        // (5) ADR-0044 — the order decides WHICH authorised write
        // survives. This is the step that refuses the F-A stale replay.
        return if (
            isNewer.isNewer(incomingMintEpoch, incomingSecret, stored.mintEpoch, stored.secret)
        ) {
            authorised
        } else {
            ReceiveVerdict.REJECT_STALE
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
        val secret = runCatching { ports.getRecord(groupIdHex) }.getOrNull()?.secret
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
     * runs the [decideAcceptReceivedSecret] authority + ordering check
     * first.
     *
     * @param mintEpoch ADR-0044 — the epoch to FILE this secret under.
     *   For a genuinely new secret that is the epoch its envelope
     *   carried (the MINTER's stamp), never the receiver's current
     *   epoch: the stamp identifies the secret, not the moment it was
     *   applied, so this device's own later re-broadcast replays the
     *   minter's stamp instead of inventing a fresher one.
     */
    public suspend fun receive(
        groupIdHex: String,
        secret: ByteArray,
        mintEpoch: ULong,
        ports: PushRoutingPorts,
    ) {
        val pushGid = ports.derivePushGid(secret)
        // Persisted as ALREADY distributed: this secret arrived off the
        // group journal, so its distribution envelope is provably on the
        // wire. Since [decideEnsureAction]'s REBROADCAST branch is open to
        // ANY holder, this flag is what keeps a receiver from re-sending an
        // envelope it just watched travel: a settled row is never retried
        // by anyone.
        ports.persist(groupIdHex, secret, mintEpoch, pushGid, distributed = true)
        subscribeOrDefer(groupIdHex, ports)
    }

    /**
     * RECEIVE side WITH the authority + ordering gate — the entry point
     * the chat receive loop uses. Reads the currently-held
     * `(secret, mintEpoch)` pair, runs [decideAcceptReceivedSecret], and
     * only on an ACCEPTING verdict does it fall through to [receive]. A
     * refusal is a pure DROP: no persist, no reverse-index write, no
     * subscribe, and no teardown — so nobody below owner rank can rotate
     * the group's `push_gid` (and cannot strand an orphan push-gw
     * subscription), and — ADR-0044 — no STALE replay can retire the
     * live one.
     *
     * A throwing `getRecord` degrades to "no stored secret", which stays
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
     * ## ADR-0044 — clearing a stale `pending` re-broadcast
     *
     * The order rule stops a stale secret from SPREADING; it does not by
     * itself stop the device still HOLDING one from re-emitting it. That
     * device keeps `S_old` in a `pending` row and re-broadcasts it on
     * every provisioning pass.
     *
     * On Android that repair is **structural, not extra machinery**:
     * [PushRoutingSecretStore] keeps the secret, the `distributed` flag
     * and the mint epoch in ONE value string, and [receive] persists with
     * `distributed = true` — so applying an accepted DIFFERENT secret
     * replaces the bytes AND clears the pending flag in the SAME atomic
     * write. After any accept there is nothing stale left to re-broadcast.
     * Pinned by a test rather than left incidental, because it is a
     * property of the row encoding (which could change) rather than of
     * this function.
     *
     * @param mintEpoch ADR-0044 — the stamp the incoming envelope carried.
     * @param ownGroupEpoch ADR-0044 — this device's OWN current group
     *   epoch (the clamp ceiling), read by the caller off the live
     *   `Group`. `null` fails closed.
     * @return the verdict, so the caller can log a refusal by NAME.
     */
    public suspend fun receiveGated(
        groupIdHex: String,
        secret: ByteArray,
        mintEpoch: ULong,
        senderSignatureKey: ByteArray?,
        owner: OwnerLookup,
        ownGroupEpoch: ULong?,
        ports: PushRoutingPorts,
    ): ReceiveVerdict {
        // ONE read for the pair — a torn (secret, epoch) read could rank a
        // superseded secret at a live epoch.
        val read: Result<StoredSecret?> = runCatching { ports.getRecord(groupIdHex) }
        val stored = read.getOrNull()
        val verdict = decideAcceptReceivedSecret(
            senderSignatureKey = senderSignatureKey,
            owner = owner,
            incomingSecret = secret,
            incomingMintEpoch = mintEpoch,
            stored = stored,
            storedUnreadable = read.isFailure,
            ownGroupEpoch = ownGroupEpoch,
            isNewer = ports.isNewer,
        )
        if (verdict.accepts) {
            try {
                // ADR-0044 — an ACCEPT_IDENTICAL re-persist must leave the
                // STORED epoch untouched. It is the one accept that clears
                // NEITHER the authority gate NOR the future clamp (it
                // short-circuits both, deliberately, so a retry stays
                // idempotent for any sender), so writing the INCOMING stamp
                // here would be an **epoch-inflation channel open to a plain
                // member**: relay the group's current secret bytes stamped
                // ULong.MAX_VALUE and every later rotation — the OWNER's
                // included — is refused REJECT_STALE forever. That is exactly
                // the permanent freeze the clamp exists to prevent, reached
                // AROUND it. The write itself is kept (it re-arms the
                // subscription and is otherwise a content no-op).
                //
                // `stored` is non-null on that branch by construction: rule
                // (1) of the gate only fires against a stored secret.
                val epochToPersist = when (verdict) {
                    ReceiveVerdict.ACCEPT_IDENTICAL -> stored?.mintEpoch ?: mintEpoch
                    else -> mintEpoch
                }
                receive(groupIdHex, secret, epochToPersist, ports)
                // ADR-0043 — the applied secret SUPERSEDED one this device
                // held, so the id it was subscribed under is now dead. Retire
                // it. Read from the pre-persist snapshot above: `receive`
                // overwrote the row in place, so after it the old bytes are
                // unrecoverable and the retiring id would be unnameable.
                if (stored != null && !stored.secret.contentEquals(secret)) {
                    retireOldPushGid(stored.secret, ports)
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
                    // ADR-0044 — the ONE place in this function that reads
                    // the group's epoch: a MINT stamps the current one. A
                    // failed read fails CLOSED (no mint, hence no push)
                    // rather than stamping a guess — an unordered secret is
                    // exactly what the v2 envelope exists to eliminate.
                    val stamp = ports.freshMintStamp()
                    if (stamp != null) {
                        val secret = ports.mintSecret()
                        val pushGid = ports.derivePushGid(secret)
                        // Persist BEFORE broadcasting, flagged NOT distributed.
                        // The order is deliberate: minting twice would diverge
                        // the group's push_gid, so a crash between the two must
                        // leave the secret durable — and the flag is what makes
                        // the un-broadcast state recoverable rather than
                        // terminal (the next `ensure` takes the REBROADCAST
                        // branch below).
                        ports.persist(groupIdHex, secret, stamp.epoch, pushGid, distributed = false)
                        broadcastAndMark(groupIdHex, stamp, secret, ports)
                    }
                }

                EnsureAction.REBROADCAST -> {
                    // Recomposed from the stored secret — never re-minted.
                    //
                    // ADR-0044 — and REPLAYING the stored stamp, never
                    // re-stamping the current epoch. This is the exact site
                    // the F-A replay travels through: re-stamping here would
                    // hand a stale secret a NEWER position than the rotation
                    // that superseded it and reproduce the finding with the
                    // ordering gate fully armed. The stamp is taken from the
                    // record itself ([StoredSecret.replayStamp]) so there is
                    // no epoch value in scope that could be passed by
                    // mistake, and the pair is read in ONE call so it cannot
                    // tear across a concurrent receive.
                    val record = ports.getRecord(groupIdHex)
                    if (record != null) {
                        broadcastAndMark(groupIdHex, record.replayStamp, record.secret, ports)
                    }
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
     *
     * ADR-0044 — [stamp] is a PARAMETER, never read from the group here.
     * Every caller has to say explicitly which epoch it is emitting, and
     * the [MintStamp] type makes it say which KIND: a MINT passes the
     * [MintStamp.Fresh] it just read, a RE-BROADCAST passes the
     * [MintStamp.Replayed] off the stored row. Sourcing it inside this
     * helper would silently re-stamp on every retry, which is the one
     * mistake that turns the whole ordering gate into a no-op.
     */
    private suspend fun broadcastAndMark(
        groupIdHex: String,
        stamp: MintStamp,
        secret: ByteArray,
        ports: PushRoutingPorts,
    ) {
        try {
            ports.broadcast(ports.composeEnvelope(stamp, secret))
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
     *
     * ADR-0044 — this is a RE-BROADCAST of an already-minted secret, so
     * it REPLAYS the stored stamp ([StoredSecret.replayStamp]). It must
     * not stamp the post-add epoch: the secret did not change, only its
     * audience did, and re-stamping would let a routine add-member
     * out-rank a rotation minted at a higher epoch.
     */
    public suspend fun redistribute(
        groupIdHex: String,
        ports: PushRoutingPorts,
    ) {
        val record = ports.getRecord(groupIdHex) ?: return
        try {
            ports.broadcast(ports.composeEnvelope(record.replayStamp, record.secret))
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

        /** ADR-0044 — this device's own `Group.epoch()` could not be read,
         *  so the new secret could not be STAMPED. Fail-closed: an
         *  unstamped mint has no position in the total order, which is
         *  exactly what the v2 envelope exists to eliminate, and guessing
         *  a stamp is worse than not rotating (the group keeps a working,
         *  ordered secret; only the eviction's re-key is deferred). */
        SKIP_UNKNOWN_EPOCH,
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
     * **The caller MUST have already published the Remove commit** —
     * C-61, and under ADR-0044 it carries TWO guarantees, not one.
     *
     * The envelope is an MLS application message: sealed at epoch N
     * (pre-removal) the evicted member is still a member of that epoch and
     * can OPEN it, learning the new secret and defeating the whole change.
     * Sealed at N+1 it is unreadable to the removed leaf — RFC 9420 forward
     * secrecy is exactly the property doing the work here.
     *
     * ADR-0044 adds the second: the new secret is STAMPED with the epoch
     * read HERE, so publishing the Remove first is also what makes the
     * rotation strictly OUT-RANK the secret it replaces. Get the order
     * wrong and the stamp is the PRE-removal epoch, which TIES with a
     * stale secret minted at that same epoch — the tie falls to the 32
     * secret bytes and the rotation may simply LOSE. The guarantee
     * degrades from "always wins" to "coin flip", silently. Use
     * [publishRemoveThenRotate] at the call site, which is where that
     * ordering is pinned by a test.
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
     *   3. ADR-0044 — read the CURRENT (post-removal) epoch as the mint
     *      stamp, INSIDE the lock and AFTER any queue wait, so a rotation
     *      that queued behind a predecessor stamps the epoch it actually
     *      mints at rather than the one it was triggered at;
     *   4. mint the NEW secret and persist it NOT distributed;
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
            val oldSecret = runCatching { ports.getRecord(groupIdHex) }.getOrNull()?.secret
            // (3) — ADR-0044: the mint stamp, read HERE, INSIDE the lock and
            // after the queue wait, so a rotation that queued behind a
            // predecessor stamps the epoch it actually mints at. C-61 is what
            // makes this stamp meaningful: the caller published the Remove
            // commit first, so this is already the POST-removal epoch and the
            // mint strictly out-ranks anything the evicted member could hold.
            // Fail-closed on an unreadable epoch.
            val stamp = if (oldSecret == null) null else ports.freshMintStamp()
            when {
                oldSecret == null -> RotationOutcome.SKIP_NO_SECRET
                stamp == null -> RotationOutcome.SKIP_UNKNOWN_EPOCH
                else -> {
                    // (4) — mint + persist PENDING. Persist before
                    // broadcasting for the same reason the mint path does: two
                    // divergent random secrets are worse than two identical
                    // envelopes.
                    val next = ports.mintSecret()
                    val pushGid = ports.derivePushGid(next)
                    ports.persist(groupIdHex, next, stamp.epoch, pushGid, distributed = false)

                    // (5) — redistribute at the POST-removal epoch. A failure
                    // leaves the row pending for the existing retry branch; it
                    // must NOT abort the local switch, which is already durable.
                    broadcastAndMark(groupIdHex, stamp, next, ports)

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
    }

    /**
     * **C-61 — publish the Remove commit, THEN rotate.** The single call
     * site for this device's own removal path, and the reason that
     * ordering is testable at all rather than being two adjacent
     * statements anyone can reorder.
     *
     * Both halves of what C-61 buys depend on [publishRemove] having
     * RETURNED before [rotate] is even started:
     *
     *   - **Forward secrecy (ADR-0043).** The rotation envelope is an MLS
     *     application message. Sealed before the Remove commit it is
     *     sealed at the pre-removal epoch, where the evicted member is
     *     still a member and can open it — learning the very secret the
     *     rotation exists to deny it.
     *   - **Recency (ADR-0044).** [rotate] stamps the new secret with
     *     `Group.epoch()` read at mint time. After the commit that is the
     *     POST-removal epoch, so the rotation strictly out-ranks the
     *     secret it replaces and a stale re-broadcast of that older secret
     *     is refused [ReceiveVerdict.REJECT_STALE] by every member. Before
     *     the commit it is the PRE-removal epoch — the same epoch the
     *     stale secret carries — so the two TIE, the tie falls to the 32
     *     secret bytes, and the rotation wins or loses by a coin flip with
     *     no signal either way.
     *
     * ADR-0043 required this ordering but enforced it only structurally,
     * by call-site sequencing, with no test on any client. ADR-0044
     * promotes it from a property that protects one guarantee to a
     * precondition of two, so it is pinned here — where a test can drive
     * both callbacks and observe which epoch the emitted envelope was
     * STAMPED at.
     *
     * The rotation is deliberately fire-and-forget on [scope]: push is
     * best-effort and a rotation failure must never unwind the membership
     * change that triggered it. "Ordered" means the rotation is not
     * STARTED until the removal has landed, not that the caller waits for
     * it.
     *
     * @param scope the (long-lived, app-scoped) scope the rotation runs
     *   on — never the caller's, which may complete first.
     * @param publishRemove applies + publishes the Remove commit,
     *   advancing the local MLS epoch. Its throw PROPAGATES — a removal
     *   that failed must not rotate.
     * @param rotate started ONLY after [publishRemove] returned. `null`
     *   skips the rotation entirely (no session / no group record —
     *   nothing to rotate for).
     * @param onRotationError receives a rotation failure. The removal
     *   itself is already durable.
     * @return whatever [publishRemove] returned, so the caller keeps its
     *   commit id (echo suppression, notices).
     */
    public suspend fun <T> publishRemoveThenRotate(
        scope: CoroutineScope,
        publishRemove: suspend () -> T,
        rotate: (suspend () -> RotationOutcome)?,
        onRotationError: ((Throwable) -> Unit)? = null,
    ): T {
        val result = publishRemove()
        if (rotate != null) {
            scope.launch {
                try {
                    rotate()
                } catch (cause: Throwable) {
                    onRotationError?.invoke(cause)
                }
            }
        }
        return result
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

    /**
     * ADR-0044 — the canonical recency order, bound in production to the
     * audited Rust `pushRoutingSecretIsNewer` FFI. NEVER re-implemented
     * in Kotlin: three clients each hand-rolling this comparison is
     * precisely the drift class that produced the F-A replay.
     *
     * A property (not a method) so it can be handed straight to the pure
     * [GroupPushRouting.decideAcceptReceivedSecret], keeping that
     * decision core FFI-free and host-testable.
     */
    public val isNewer: GroupPushRouting.PushRoutingSecretIsNewer

    /**
     * ADR-0044 — compose the E2EE distribution-envelope bytes for
     * [secret], stamped with [stamp].
     *
     * [stamp] is a [GroupPushRouting.MintStamp], not a bare `ULong`, so a
     * caller cannot reach this method without stating whether the epoch
     * is a FRESH read of the live group ([freshMintStamp]) or a REPLAY of
     * the epoch a stored secret was minted under
     * ([GroupPushRouting.StoredSecret.replayStamp]). Re-stamping a
     * re-broadcast is the one mistake that silently voids the whole
     * ordering gate, and it is invisible in a `ULong`.
     */
    public fun composeEnvelope(
        stamp: GroupPushRouting.MintStamp,
        secret: ByteArray,
    ): ByteArray

    /**
     * ADR-0044 — read this device's OWN current MLS group epoch as a
     * FRESH mint stamp, or `null` when it cannot be read (fail-closed: a
     * mint that cannot be stamped is not performed).
     *
     * The ONLY producer of a [GroupPushRouting.MintStamp.Fresh], and
     * therefore the only way a new epoch value enters the system.
     * Deliberately a suspending PORT rather than a caller-supplied value:
     * the rotation path must read the epoch INSIDE its serializing lock,
     * AFTER any queue wait, so a rotation queued behind a predecessor
     * stamps the epoch it actually mints at — a value resolved at the
     * call site would be read before the wait.
     */
    public suspend fun freshMintStamp(): GroupPushRouting.MintStamp.Fresh?

    /** Has this device a persisted secret for `groupIdHex`? */
    public fun hasSecret(groupIdHex: String): Boolean

    /**
     * ADR-0044 — read the persisted secret for `groupIdHex` TOGETHER
     * with the epoch it was minted under, or `null` when this device
     * holds none.
     *
     * One read for the PAIR, deliberately: two separate reads could tear
     * across a concurrent receive (secret from before the write, epoch
     * from after), and a torn pair composes an envelope claiming a live
     * epoch for a superseded secret — the F-A shape itself.
     */
    public fun getRecord(groupIdHex: String): GroupPushRouting.StoredSecret?

    /** Has the held secret's distribution envelope been confirmed on
     *  the wire? `true` when nothing is held (there is nothing pending
     *  to re-send) and for a LEGACY row written before the flag
     *  existed. Drives [GroupPushRouting.EnsureAction.REBROADCAST]. */
    public fun isDistributed(groupIdHex: String): Boolean

    /** Persist the secret (encrypted at rest) AND the
     *  `push_gid -> (account, group)` reverse-index entry, together
     *  with whether its distribution envelope is already on the wire
     *  ([distributed]) and the ADR-0044 [mintEpoch] it was minted under.
     *  The owner mint passes `false`; a received secret passes `true`.
     *
     *  [mintEpoch] is a bare `ULong` rather than a
     *  [GroupPushRouting.MintStamp] on purpose: a RECEIVE files the epoch
     *  the accepted ENVELOPE carried, which is neither a fresh read nor a
     *  replay of anything stored. The stamp type governs COMPOSING, which
     *  is where the F-A defect lives. */
    public fun persist(
        groupIdHex: String,
        secret: ByteArray,
        mintEpoch: ULong,
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
    private val groupEpochFn: suspend () -> ULong?,
) : PushRoutingPorts {

    override fun mintSecret(): ByteArray =
        ByteArray(PushRoutingSecretStore.SECRET_LENGTH).also { SecureRandom().nextBytes(it) }

    override fun derivePushGid(secret: ByteArray): ByteArray = ffiDerivePushGid(secret)

    /** ADR-0044 — the audited Rust order, verbatim. No Kotlin
     *  re-derivation exists anywhere in this client. */
    override val isNewer: GroupPushRouting.PushRoutingSecretIsNewer =
        GroupPushRouting.PushRoutingSecretIsNewer(::ffiPushRoutingSecretIsNewer)

    override fun composeEnvelope(
        stamp: GroupPushRouting.MintStamp,
        secret: ByteArray,
    ): ByteArray = composePushRoutingSecret(stamp.epoch, secret)

    /** ADR-0044 — the live `Group.epoch()`, or `null` when it could not be
     *  read (fail-closed: an unstamped mint is never performed). */
    override suspend fun freshMintStamp(): GroupPushRouting.MintStamp.Fresh? =
        groupEpochFn()?.let { GroupPushRouting.MintStamp.Fresh(it) }

    override fun hasSecret(groupIdHex: String): Boolean =
        PushRoutingSecretStore.has(context, accountIdB64, groupIdHex)

    override fun getRecord(groupIdHex: String): GroupPushRouting.StoredSecret? =
        PushRoutingSecretStore.entry(context, accountIdB64, groupIdHex)
            ?.let { GroupPushRouting.StoredSecret(it.secret, it.mintEpoch) }

    override fun isDistributed(groupIdHex: String): Boolean =
        PushRoutingSecretStore.isDistributed(context, accountIdB64, groupIdHex)

    override fun persist(
        groupIdHex: String,
        secret: ByteArray,
        mintEpoch: ULong,
        pushGid: ByteArray,
        distributed: Boolean,
    ) {
        PushRoutingSecretStore.put(
            context,
            accountIdB64,
            groupIdHex,
            secret,
            distributed,
            mintEpoch,
        )
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
