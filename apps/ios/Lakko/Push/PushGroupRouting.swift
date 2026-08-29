// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore
import Security

/// ADR-0042 orchestration — mint / receive / derive / subscribe / notify
/// the opaque per-group push routing id (`push_gid`), closing the
/// invariant-#5 push social-graph leak — plus the ADR-0043 rotation of that
/// id when a member is REMOVED (see the rotation section at the bottom of
/// this file for why removal is the only event that rotates).
///
/// Mirror of `apps/web/src/push/group-routing.ts`.
///
/// ## The load-bearing rule: OWNER mints, everyone else RECEIVES
///
/// `push_routing_secret` is 32 RANDOM bytes, so two members minting
/// independently would diverge and break routing (their `push_gid`s would
/// differ and the notify JOIN would never match). Therefore exactly ONE
/// source mints:
///
///   * Only the ADR-0022 group **OWNER** mints, once, and persists it
///     (``ensureProvisioned(accountIdB64:groupIdHex:group:identity:broadcast:)``).
///     It then broadcasts the secret to the group over an E2EE MLS
///     application message (the DS sees ciphertext — invariant #2).
///   * Everyone else ONLY receives + persists the secret from that
///     envelope
///     (``receiveSecret(accountIdB64:groupIdHex:secret:group:senderLeaf:)``)
///     — they never mint.
///
/// The receive side enforces the same rule rather than trusting it: a
/// *changed* secret is accepted only when the MLS-authenticated sender leaf
/// resolves to a signature key the ADR-0022 role table authorises
/// (`lakko-security` MEDIUM-1). Otherwise any single member could
/// unilaterally rotate the group's routing id and strand an orphan push-gw
/// subscription per churn.
///
/// The authorised set is **owner + admins, with the owner taking
/// priority** — expressed as *who may overwrite what*, so no authority
/// level is ever persisted:
///
///   * the OWNER may always set the secret (rotate it, whatever is
///     stored and in whatever arrival order);
///   * an ADMIN may **seed a hole** — set the FIRST-ever secret this
///     device holds — AND (ADR-0043, ratified option (b)) **overwrite** an
///     established one;
///   * a plain member may only re-send the byte-identical value.
///
/// Admitting admins as seeders is what keeps delivery working: ADR-0022
/// lets owner OR admin add a member, and `redistribute` fires after any
/// add-member commit, so a member added by a non-owner admin learns the
/// secret from that admin's re-broadcast (the owner only broadcasts at
/// mint time). Admitting admins as **rotators** (ADR-0043) is what makes
/// rotation-on-removal reachable at all, since ADR-0022 also lets an admin
/// commit the Remove: refusing the overwrite left an admin able to evict a
/// member but unable to retire that member's routing id, so the eviction
/// completed with the ex-member's wake stream intact.
///
/// The cost of the admin tier is recorded honestly in ADR-0043 § fork (b):
/// it widens G-36 residual 3 from "a malicious admin can seed a secret to a
/// member who holds none" to "a malicious admin can churn the group's
/// routing id at will". The capability gain is small (an admin can already
/// evict members outright) and the churn is a wake outage, never a
/// downgrade — a member without the current secret still DEFERS, it never
/// falls back to the leaky `group_id`.
///
/// **Rotation never MINTS.** Option (b) widened OVERWRITE authority, never
/// MINT authority: ``rotateSecret(accountIdB64:groupIdHex:ownRole:broadcast:)``
/// is a no-op on a device that holds no secret, so an admin can never become
/// a second minter and diverge the group's `push_gid`.
/// See ``classifyIncomingSecret(incoming:stored:senderKey:authority:)``.
///
/// ## A mint is not done until the group has been told
///
/// The owner persists before it broadcasts (minting twice would diverge the
/// group's routing id — far worse than sending one envelope twice), so a
/// broadcast that fails leaves a durably stored secret nobody else knows.
/// The store therefore carries a durable **`distributed`** flag beside each
/// secret, and the retry branch of
/// ``ensureProvisioned(accountIdB64:groupIdHex:group:identity:broadcast:)``
/// re-broadcasts whenever it is still `false`. Unlike the ADR-0040
/// commit-outbox this needs no stored bytes: the distribution envelope is
/// recomputable from the secret at any time
/// (``composeSecretEnvelope(secret:)``), so recording *whether* it landed is
/// the entire durable state. Re-broadcasting is idempotent for every member
/// because the receive gate accepts a byte-identical secret from anyone.
///
/// The same one bit covers the *post-add-member*
/// ``redistribute(accountIdB64:groupIdHex:broadcast:)``, which is where the
/// per-GROUP flag would otherwise fall short of a per-MEMBER guarantee: that
/// broadcast runs against a row the mint already settled, for the benefit of
/// the ONE member just added, so a failure must un-settle the row
/// (``PushRoutingStore/markPending(accountIdB64:groupIdHex:matching:)``) —
/// else the flag stays `true`, no sweep re-sends, and that member defers push
/// forever.
///
/// **The retry is NOT owner-gated, and must not be.** Only the MINT is:
/// the secret is random, so two independent mints diverge the group's
/// `push_gid`. Re-sending bytes the device already holds picks nothing —
/// the receive gate accepts a byte-identical secret from ANY sender before
/// authority is even consulted — so any holder may retry. That matters
/// because ADR-0022 lets an ADMIN add a member: the admin's `redistribute`
/// runs, and if it fails only the ADMIN's row is left pending. An owner-only
/// retry branch would make that flag inert and the newly-added member would
/// never be sent the secret by anyone.
///
/// ## Never subscribe under the leaky `group_id`
///
/// The whole point of the ADR is that push-gw + APNs/FCM must not see the
/// DS journal key. So a member that does NOT yet hold the secret
/// **defers** its push subscription (transient no-push until the secret
/// arrives) — it never falls back to subscribing under `group_id`
/// (``subscribeUnderPushGidBestEffort(accountIdB64:groupIdHex:)`` is a no-op
/// when no secret is held). The publisher always holds the secret (it is a
/// member), so notify always has a `push_gid`.
///
/// ## Concurrency
///
/// EVERY path that READ-MODIFY-WRITEs a group's routing row — receive,
/// rotate, provision, redistribute — runs inside one per-`(account, group)`
/// **serializing** chain (``withRoutingLock(accountIdB64:groupIdHex:_:)``).
/// One lock rather than four, because the four paths race each OTHER, not
/// merely themselves: a rotation parked in its broadcast and an inbound peer
/// secret both hold a snapshot of the same row, and interleaving them can
/// leave an intermediate `push_gid` armed but never retired.
///
/// A second trigger **queues and then runs in full**; it is never folded
/// into the section already running. Coalescing is correct only when both
/// triggers want the identical outcome, and silently wrong when they answer
/// two different events — see
/// ``rotateSecret(accountIdB64:groupIdHex:ownRole:mintEpoch:broadcast:)``
/// for the case where a fold voids the whole ADR for one member.
///
/// **Provisioning keeps its own additional guard, and it is a different
/// question.** The chain answers "may this section observe a half-applied
/// row"; the ``inflight`` set answers "should this redundant trigger run at
/// all" — a concurrent legacy-sweep and chat-view onAppear want the identical
/// outcome ("this group has a secret"), so the second is dropped outright
/// rather than queued behind the first and made to re-do it.
///
/// Sections hold the lock across their network round-trips (the E2EE journal
/// broadcast, the push-gw subscribe / unsubscribe), so an inbound envelope
/// for a group with a rotation in flight waits for it. That is the intended
/// semantics, not a cost to be trimmed: those are exactly the two writers
/// that must not interleave.
///
/// ### …but every one of those round-trips is BOUNDED
///
/// Holding the row across network I/O is only safe while that I/O cannot run
/// forever. Unbounded, ONE hung request wedges the row **permanently**: every
/// later envelope queues behind it, so this device never applies a peer's
/// rotation and keeps notifying — and stays subscribed to — a routing id an
/// evicted member can still derive. An ADR-0043 guarantee lost to a stalled
/// socket, with no error raised anywhere.
///
/// So each awaited network call under the lock runs inside
/// ``withNetworkTimeout(_:_:)``, bounded by ``networkCallTimeout``. The bound
/// is per CALL rather than per section, deliberately: a whole-section
/// deadline would abort at an arbitrary point and invent a new half-applied
/// shape, whereas a per-call deadline lands in that call's **already
/// existing** failure handler, so no new state exists anywhere —
///
///   * a **broadcast** timeout leaves the row `pending`, exactly as a DS
///     error does, and the any-holder re-broadcast branch re-sends it;
///   * a **subscribe** timeout is swallowed, exactly as an unreachable
///     push-gw is, and the next
///     ``ensureProvisioned(accountIdB64:groupIdHex:group:identity:broadcast:)``
///     re-subscribes at its tail;
///   * an **unsubscribe** timeout is swallowed, exactly as an unreachable
///     push-gw is, leaving a stale push-gw row that nothing ever notifies
///     again — already the documented cost in ``retireOldPushGid(oldSecret:)``.
///
/// A rotation therefore occupies the row for at most three deadlines instead
/// of forever, and every expiry leaves it in a state a later trigger repairs.
///
/// ### The availability decision, stated because the trade is real
///
/// A deadline is a choice about what to do when the network stops answering,
/// and this one resolves in TWO directions at once — which is why "fail open
/// or fail closed" has to be asked of the OPERATION and the ROW separately:
///
///   * **the operation fails CLOSED.** An expired call is treated as FAILED,
///     never as optimistically succeeded. A timed-out broadcast is never
///     flagged distributed; a timed-out unsubscribe never claims the old id
///     was retired. Nothing that decides *who may write* (the ADR-0042
///     authority gate) or *which write survives* (the ADR-0044 order) is
///     relaxed — both still run in full, inside the lock, against a fresh
///     read. A deadline can only turn a would-be success into a
///     treated-as-failure; it can never admit an envelope the gate refused,
///     persist an unordered secret, or mark an undelivered one distributed.
///   * **the row fails OPEN.** The lock is released and the next queued
///     section runs. That is the entire point of the change: a wedged row is
///     strictly worse than a repeated round-trip, because the work queued
///     behind it is precisely the peer rotation that retires an evicted
///     member's id.
///
/// This is **not** the ADR-0033 fail-open. That one relaxes an anti-abuse
/// GATE on a store error, reasoning that failing closed there would be
/// self-inflicted denial of service. Here no gate is relaxed at all: the
/// fail-open governs only how long one section may occupy the row, and its
/// cost is an extra idempotent retry, paid on a network that is already
/// failing. The confidentiality- and ordering-relevant properties are
/// untouched in both directions.
///
/// `@MainActor` so the owner-role check + the E2EE `broadcast` (which runs
/// `Group.createApplicationMessage` in ``ChatRepository``) stay on the same
/// actor that owns the MLS `Group` handle.
@MainActor
public final class PushGroupRouting {

    /// `push_routing_secret` length (bytes) — mirrors the Rust
    /// `PUSH_ROUTING_SECRET_LEN`.
    public static let secretLen: Int = 32

    /// ADR-0044 — byte length of the big-endian `mint_epoch` field; mirrors
    /// the Rust `PUSH_ROUTING_SECRET_MINT_EPOCH_LEN`.
    public static let mintEpochLen: Int = 8

    /// Total distribution-envelope length (bytes): the 29-byte domain
    /// separator + 1 version byte + the 8-byte big-endian `mint_epoch` + the
    /// 32-byte secret. Used only as a cheap receive-side pre-filter before
    /// the authoritative FFI decode. Mirrors `lakko_contacts::push_routing`
    /// (29 + 1 + 8 + 32 = 70). The v1 envelope was 62 bytes and is refused
    /// outright — ADR-0044 is a hard flag day, because a v1 envelope carries
    /// no `mint_epoch` and so has no position in the total order.
    public static let envelopeLen: Int = 29 + 1 + mintEpochLen + secretLen

    /// First byte of the envelope domain separator (`'l'` of
    /// `"lakko/push-routing-secret\0"`). The cheap pre-filter; the FFI
    /// `decodePushRoutingSecret` is the authoritative fail-closed check.
    private static let envelopeFirstByte: UInt8 = 0x6c

    /// Default deadline (seconds) for ONE network round-trip taken under the
    /// serializing routing-row lock — see the "every one of those round-trips
    /// is BOUNDED" section of the type doc.
    ///
    /// Chosen, not measured: comfortably above a healthy mobile round-trip,
    /// and below `URLSession`'s own 60 s default request timeout so a stalled
    /// connection is bounded by THIS lock's needs rather than by the
    /// transport's. It caps a rotation's worst-case hold on the row at three
    /// deadlines (broadcast + subscribe + unsubscribe). The exact value is not
    /// load-bearing — being finite is.
    public static let defaultNetworkCallTimeout: TimeInterval = 15

    /// Deadline for one network round-trip under the routing-row lock.
    ///
    /// Injectable ONLY so tests can pick a short one: unlike the Android
    /// mirror — where `runTest`'s virtual clock advances a `withTimeoutOrNull`
    /// instantly and the bound can stay a plain constant — XCTest has no
    /// virtual clock, so a test of the production 15 s would sleep for 15 real
    /// seconds. Production always takes ``defaultNetworkCallTimeout``.
    private let networkCallTimeout: TimeInterval

    private let store: PushRoutingStore

    /// Subscribe seam — `(push_gid) -> ()`. Defaults to the real push-gw
    /// call (device pseudonym from the Keychain + ``PushClient``);
    /// injectable so tests can spy on the target without touching the
    /// network (mirror of the web `PushClient` `vi.mock`).
    private let subscribeUnderPushGid: (Data) async throws -> Void

    /// Unsubscribe seam — `(push_gid) -> ()`. ADR-0043: the counterpart the
    /// rotation needs to retire a SUPERSEDED id. Defaults to the real
    /// `POST /push/group/unsubscribe` call, which had **zero call sites**
    /// before this ADR (the wrapper existed on all three clients and was
    /// never invoked); injectable so tests can spy on the target.
    private let unsubscribeUnderPushGid: (Data) async throws -> Void

    /// Per-group in-flight provisioning keys, so a concurrent legacy-sweep
    /// + onAppear for the same group mint the random secret exactly once.
    private var inflight: Set<String> = []

    /// Per-`(account, group)` chain tail — the **serializing** lock every
    /// read-modify-write of a routing row runs inside. The Swift counterpart
    /// of the Android `ConcurrentHashMap<String, Mutex>`.
    ///
    /// The value is the tail `Task` the next caller chains behind; it is
    /// type-erased to `Void` so ONE queue can hold sections that return
    /// different things (a receive returns nothing, a rotation returns a
    /// ``PushRotationOutcome``).
    ///
    /// **A second caller WAITS and then runs in full; it is never folded into
    /// the section already running.** Coalescing is only correct when both
    /// callers want the same outcome, and it is silently wrong when they
    /// answer different events. The sharpest case is rotation: two triggers
    /// can observe two DIFFERENT removals (this device's own
    /// `removeMemberFromGroup` and a concurrent poll-loop diff). A rotation
    /// seals its envelope at the epoch it starts in, then spends three network
    /// round-trips (broadcast, subscribe, unsubscribe) in flight. A removal
    /// committed inside that window and folded into the running rotation would
    /// be answered by an envelope the *new* evictee was still a member of — so
    /// it opens the envelope, learns the current secret, keeps a live
    /// subscription, and nothing ever rotates again (the removing device's own
    /// poll baseline is already post-removal, so no shrink is ever seen). The
    /// single load-bearing property of ADR-0043 would be void for that member.
    /// Serializing rules it out: the successor runs in full, reading the
    /// predecessor's output as its own input, so every intermediate id is
    /// retired and every caller observes a real result.
    ///
    /// The tail is cleared only by the caller that is STILL the tail when its
    /// own section finishes, so a queued successor is never evicted from the
    /// chain (which would let the caller after it start beside it).
    private var routingChain: [String: Task<Void, Never>] = [:]

    /// The per-`(account, group)` lock key. `accountIdB64` is base64 and
    /// `groupIdHex` is hex, so neither can contain the separator — a NUL
    /// written as an ESCAPE, never a literal, so the file stays text to
    /// `grep` and `git diff` (the ADR-0042 `lakko-security` MEDIUM-3 class).
    private static func lockKey(accountIdB64: String, groupIdHex: String) -> String {
        accountIdB64 + "\u{0}" + groupIdHex
    }

    /// Run `body` as the sole holder of a group's routing row.
    ///
    /// Serializing, not coalescing: `body` always runs, after everything
    /// already queued for that `(account, group)` has finished, and observes
    /// their writes. That is what lets a section read the row and then write
    /// it without a concurrent section having moved it in between.
    ///
    /// - Note: `body` must not itself call a locked entry point
    ///   (``receiveSecret(accountIdB64:groupIdHex:secret:mintEpoch:group:senderLeaf:)``,
    ///   ``rotateSecret(accountIdB64:groupIdHex:ownRole:mintEpoch:broadcast:)``,
    ///   ``ensureProvisioned(accountIdB64:groupIdHex:group:identity:broadcast:)``,
    ///   ``redistribute(accountIdB64:groupIdHex:broadcast:)``) — the chain is
    ///   not reentrant, so that would deadlock. The leaf helpers the sections
    ///   DO call — ``persistSecret(accountIdB64:groupIdHex:secret:distributed:mintEpoch:)``,
    ///   ``subscribeUnderPushGidBestEffort(accountIdB64:groupIdHex:)``,
    ///   ``retireOldPushGid(oldSecret:)``, ``pushGid(accountIdB64:groupIdHex:)``
    ///   — are deliberately lock-FREE for exactly this reason, and must stay
    ///   that way. The injected `subscribe` / `unsubscribe` seams and the
    ///   `broadcast` closure reach `PushClient` and `ChatClient` respectively,
    ///   neither of which calls back into this type (`ChatClient`'s only
    ///   reference is the `nonisolated static` ``tryDecodeSecretEnvelope(_:)``
    ///   classifier, which touches no state).
    private func withRoutingLock<T>(
        accountIdB64: String,
        groupIdHex: String,
        _ body: @escaping @MainActor () async -> T
    ) async -> T {
        let key = Self.lockKey(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
        // Read the tail and replace it with NO `await` in between: `@MainActor`
        // cannot interleave another caller across a straight-line region, so
        // two callers can never observe the SAME predecessor and run beside
        // each other.
        let predecessor = routingChain[key]
        let run = Task { @MainActor () -> T in
            // Wait for everything already queued for this group. `Never`-
            // failing and best-effort throughout, so this can neither throw
            // nor hang on a predecessor's failure.
            _ = await predecessor?.value
            return await body()
        }
        // The erased tail: completes exactly when `run` does, so a successor
        // chaining on it waits for this section's writes to land.
        let tail = Task { @MainActor in _ = await run.value }
        routingChain[key] = tail
        let outcome = await run.value
        // Tail-guarded release — a successor that queued behind this section is
        // the tail now, and dropping it would reopen the race for the caller
        // after it.
        if routingChain[key] == tail { routingChain.removeValue(forKey: key) }
        return outcome
    }

    // MARK: - Bounding the network I/O taken under the routing-row lock

    /// Raised by ``withNetworkTimeout(_:_:)`` when a network round-trip taken
    /// under the routing-row lock outlives ``networkCallTimeout``.
    ///
    /// It is never a new failure MODE — every call site folds it into that
    /// call's already-existing handler (a broadcast leaves the row pending, a
    /// subscribe / unsubscribe is swallowed), so a hung socket produces
    /// exactly the state an outright network error already produced.
    public struct NetworkCallTimedOut: Error {
        /// Which call expired. Low-cardinality, for the log — never key
        /// material.
        public let what: String
    }

    /// One-shot "who got there first" flag for ``withNetworkTimeout(_:_:)``.
    /// `@MainActor`-confined, so the operation and the timer can never both
    /// observe it unclaimed and double-resume the continuation.
    @MainActor
    private final class RaceLatch {
        private var claimed = false
        /// `true` for exactly one caller — the winner of the race.
        func claim() -> Bool {
            if claimed { return false }
            claimed = true
            return true
        }
    }

    /// Holds the timer task so the winner can cancel the loser rather than
    /// leaving a sleeper parked for the rest of the deadline.
    @MainActor
    private final class TaskBox {
        var task: Task<Void, Never>?
        func cancel() { task?.cancel() }
    }

    /// Run one network round-trip with a deadline, so it cannot hold the
    /// serializing routing-row lock forever.
    ///
    /// **Abandons rather than merely cancelling.** On expiry the caller is
    /// resumed immediately with ``NetworkCallTimedOut`` and the operation is
    /// left to unwind on its own; `work.cancel()` is a courtesy that lets a
    /// cooperative operation (`URLSession`'s async API is one) stop early.
    /// Waiting for cancellation to be *observed* would reopen the exact hole
    /// this closes for any operation that does not observe it — and the
    /// requirement is that the row is freed, not that the socket is tidy.
    /// This is the one place the two platforms differ in mechanism: Kotlin's
    /// `withTimeoutOrNull` cancels and awaits the unwind, which is that
    /// language's idiom and is sufficient for a cancellable HTTP client;
    /// Swift has no `withTimeout` at all, so the race had to be written by
    /// hand, and writing the abandoning one is no larger than the awaiting
    /// one while being strictly stronger.
    ///
    /// Abandoning is safe HERE specifically because nothing in the routing
    /// row is written by the operation itself: `broadcast`, `subscribe` and
    /// `unsubscribe` are all remote calls whose only local effect happens in
    /// the caller AFTER they return. A late-completing abandoned call
    /// therefore cannot write anything behind a successor's back.
    ///
    /// It is also why every caller passes its seam in through a **capture
    /// list** (`{ [subscribeUnderPushGid] in … }`) rather than reaching
    /// through `self`: the `work` task can outlive this call by an unbounded
    /// margin, and an implicit `self` capture would keep the whole
    /// ``PushGroupRouting`` — and its store — alive for exactly as long as
    /// the hang lasts.
    ///
    /// - Parameters:
    ///   - what: low-cardinality tag naming the call, for the log.
    ///   - operation: the network round-trip. Its own throw propagates
    ///     unchanged, so a real error and a deadline reach the same handler.
    private func withNetworkTimeout(
        _ what: String,
        _ operation: @escaping @MainActor () async throws -> Void
    ) async throws {
        // Floor the deadline so a degenerate injected value cannot make the
        // timer fire before the operation is even started.
        let deadlineNanos = UInt64(max(networkCallTimeout, 0.001) * 1_000_000_000)
        let latch = RaceLatch()
        let timer = TaskBox()
        defer { timer.cancel() }
        try await withCheckedThrowingContinuation {
            (continuation: CheckedContinuation<Void, Error>) in
            let work = Task { @MainActor in
                do {
                    try await operation()
                    if latch.claim() { continuation.resume() }
                } catch {
                    if latch.claim() { continuation.resume(throwing: error) }
                }
            }
            timer.task = Task { @MainActor in
                try? await Task.sleep(nanoseconds: deadlineNanos)
                guard latch.claim() else { return }
                work.cancel()
                continuation.resume(throwing: NetworkCallTimedOut(what: what))
            }
        }
    }

    /// `nonisolated` so the initializer is callable as a default argument
    /// from a nonisolated context (``ChatRepository/init``); it only assigns
    /// stored properties.
    public nonisolated init(
        networkCallTimeout: TimeInterval = PushGroupRouting.defaultNetworkCallTimeout,
        store: PushRoutingStore = PushRoutingStore(),
        subscribeUnderPushGid: @escaping (Data) async throws -> Void = { pushGid in
            let devicePseudonym = try DevicePushId.getOrCreate()
            _ = try await PushClient().subscribeGroup(
                devicePseudonym: devicePseudonym,
                groupPseudonym: pushGid
            )
        },
        unsubscribeUnderPushGid: @escaping (Data) async throws -> Void = { pushGid in
            let devicePseudonym = try DevicePushId.getOrCreate()
            _ = try await PushClient().unsubscribeGroup(
                devicePseudonym: devicePseudonym,
                groupPseudonym: pushGid
            )
        }
    ) {
        self.networkCallTimeout = networkCallTimeout
        self.store = store
        self.subscribeUnderPushGid = subscribeUnderPushGid
        self.unsubscribeUnderPushGid = unsubscribeUnderPushGid
    }

    // MARK: - Primitives (mirror of group-routing.ts helpers)

    /// Mint a fresh `push_routing_secret` (32 CSPRNG bytes). OWNER ONLY —
    /// everyone else receives it over the distribution envelope.
    public nonisolated static func mintSecret() -> Data? {
        var bytes = Data(count: secretLen)
        let status = bytes.withUnsafeMutableBytes { ptr -> Int32 in
            guard let base = ptr.baseAddress else { return errSecAllocate }
            return SecRandomCopyBytes(kSecRandomDefault, secretLen, base)
        }
        return status == errSecSuccess ? bytes : nil
    }

    /// Derive the stable opaque `push_gid` (32 bytes) from a secret via the
    /// audited FFI keyed-BLAKE3 derivation. Deterministic: every member
    /// that holds the same secret derives the same `push_gid`.
    public nonisolated static func derivePushGid(secret: Data) -> Data? {
        try? LakkoCore.derivePushGid(secret: secret)
    }

    /// A decoded push-routing-secret distribution envelope — the 32-byte
    /// secret together with the `mintEpoch` it was stamped under (ADR-0044).
    ///
    /// Carried as a PAIR from decode to gate to store: the stamp identifies
    /// the secret, so separating them anywhere on that path is what would
    /// let a superseded secret be judged at a live epoch.
    public struct DecodedSecretEnvelope: Equatable {
        /// The 32-byte `push_routing_secret`.
        public let secret: Data
        /// The minter's recency stamp — the primary key of the canonical
        /// total order.
        public let mintEpoch: UInt64

        public init(secret: Data, mintEpoch: UInt64) {
            self.secret = secret
            self.mintEpoch = mintEpoch
        }
    }

    /// Compose the E2EE distribution-envelope bytes for a secret (FFI),
    /// stamped with the epoch that secret was MINTED under.
    ///
    /// **`mintEpoch` is NOT "now".** It is stamped once, at mint time, and
    /// then travels with the secret forever: every re-broadcast recomposes
    /// the envelope from the STORED secret and must replay the STORED epoch.
    /// Re-stamping the current epoch on a re-broadcast would hand a stale
    /// secret a newer position in the order than the live rotation it was
    /// superseded by — reproducing the F-A replay (G-36 residual 9) with the
    /// ordering gate fully in place. See ADR-0044 § "The mint epoch must be
    /// PERSISTED and re-emitted verbatim".
    public nonisolated static func composeSecretEnvelope(
        mintEpoch: UInt64,
        secret: Data
    ) throws -> Data {
        try composePushRoutingSecret(mintEpoch: mintEpoch, secret: secret)
    }

    /// Attempt to decode an MLS application-message plaintext as a
    /// push-routing-secret distribution envelope. Returns the 32-byte
    /// secret plus its `mintEpoch`, or `nil` when the bytes are not this
    /// envelope (fail-closed — the authoritative check is the FFI codec's
    /// domain + version + exact-length gate, which also refuses every
    /// retired v1 envelope with `UnknownVersion`). A cheap length/first-byte
    /// pre-filter avoids an FFI call on every chat message.
    public nonisolated static func tryDecodeSecretEnvelope(
        _ bytes: Data
    ) -> DecodedSecretEnvelope? {
        guard bytes.count == envelopeLen, bytes.first == envelopeFirstByte else {
            return nil
        }
        // Not a valid push-routing envelope (domain / version / length
        // mismatch) → `nil` so the caller runs the normal chat parse.
        guard let decoded = try? decodePushRoutingSecret(plaintext: bytes) else { return nil }
        return DecodedSecretEnvelope(secret: decoded.secret, mintEpoch: decoded.mintEpoch)
    }

    /// The canonical recency order, as the audited Rust core defines it
    /// once: `(mintEpoch, secretBytes)`, epoch first, secret bytes
    /// lexicographically on an exact tie, **strictly greater wins**.
    ///
    /// A thin passthrough to the FFI `pushRoutingSecretIsNewer` — deliberately
    /// NOT reimplemented in Swift. Three clients each hand-rolling this
    /// comparison is precisely the drift class that produced the F-A replay,
    /// so the order lives in the core and every platform routes through it.
    public nonisolated static func isNewer(
        incomingEpoch: UInt64,
        incomingSecret: Data,
        storedEpoch: UInt64,
        storedSecret: Data
    ) -> Bool {
        pushRoutingSecretIsNewer(
            incomingEpoch: incomingEpoch,
            incomingSecret: incomingSecret,
            storedEpoch: storedEpoch,
            storedSecret: storedSecret
        )
    }

    /// ADR-0044 — read the group's CURRENT MLS epoch: the stamp source for a
    /// mint and the ceiling for the receive-side clamp.
    ///
    /// `nil` when the read throws (a torn group / poisoned handle), which
    /// every caller treats as fail-closed: a mint that cannot be stamped is
    /// not performed, and an incoming envelope that cannot be clamped is
    /// refused.
    public nonisolated static func readGroupEpoch(_ group: Group) -> UInt64? {
        try? group.epoch()
    }

    // MARK: - Persist + resolve

    /// Persist a group's secret + its `distributed` flag
    /// (device-key-encrypted) AND its `push_gid → (account, group_id)`
    /// reverse-index entry. Idempotent. Returns the derived `push_gid`
    /// bytes, or `nil` when the derivation fails (should not happen for a
    /// 32-byte secret).
    ///
    /// - Parameter distributed: whether the group has already been told this
    ///   secret. The owner-mint path persists `false` FIRST and flips it
    ///   only once the broadcast lands; the receive path persists `true`
    ///   (the envelope's arrival IS the proof it travelled).
    /// - Parameter mintEpoch: ADR-0044 — the epoch this secret was MINTED
    ///   under, not the current one. On a receive it is the epoch the
    ///   accepted envelope carried, so a re-broadcast from this device
    ///   replays the minter's stamp rather than inventing a fresher one.
    @discardableResult
    public func persistSecret(
        accountIdB64: String,
        groupIdHex: String,
        secret: Data,
        distributed: Bool,
        mintEpoch: UInt64
    ) -> Data? {
        guard let pushGid = Self.derivePushGid(secret: secret) else { return nil }
        store.putRouting(
            accountIdB64: accountIdB64,
            groupIdHex: groupIdHex,
            secret: secret,
            pushGidHex: Self.hex(pushGid),
            distributed: distributed,
            mintEpoch: mintEpoch
        )
        return pushGid
    }

    /// Look up the `push_gid` bytes for a group, or `nil` when this device
    /// does not (yet) hold the secret. Used by the notify path (publisher)
    /// and the subscribe path.
    public func pushGid(accountIdB64: String, groupIdHex: String) -> Data? {
        guard let secret = store.getSecret(
            accountIdB64: accountIdB64, groupIdHex: groupIdHex) else {
            return nil
        }
        return Self.derivePushGid(secret: secret)
    }

    /// Reverse-map a `push_gid` (lowercase hex) to `(account, group_id)`.
    /// Exposed for the Sprint-4 NSE decrypt reverse-map (Sprint 3 only
    /// POPULATES this via ``persistSecret``).
    public func lookupRoute(pushGidHex: String)
        -> (accountIdB64: String, groupIdHex: String)? {
        store.lookupRoute(pushGidHex: pushGidHex)
    }

    // MARK: - Subscribe / receive / redistribute

    /// Subscribe this device to push wakes for a group UNDER ITS
    /// `push_gid`. A member that does not hold the secret **defers** — it
    /// never subscribes under the leaky `group_id`. Best-effort: failures
    /// are logged and swallowed (push is a UX nicety; foreground polling
    /// still works).
    public func subscribeUnderPushGidBestEffort(
        accountIdB64: String,
        groupIdHex: String
    ) async {
        guard let pushGid = pushGid(accountIdB64: accountIdB64, groupIdHex: groupIdHex) else {
            // No secret held yet → DEFER. Never subscribe under group_id.
            return
        }
        do {
            // BOUNDED: this runs under the serializing routing-row lock, so a
            // hung push-gw would otherwise wedge the row and stop this device
            // ever applying a peer's rotation. A deadline lands in the same
            // swallow below as an unreachable push-gw, and the next
            // `ensureProvisioned` re-subscribes at its tail.
            try await withNetworkTimeout("subscribe") { [subscribeUnderPushGid] in
                try await subscribeUnderPushGid(pushGid)
            }
        } catch {
            NSLog("push_gid subscribe failed: \(error)")
        }
    }

    /// ADR-0045 UX §6.2.8 item 5 — retire this device's OWN subscription for a
    /// group whose membership has ended.
    ///
    /// The THIRD caller of ``retireOldPushGid(oldSecret:)``, and the one
    /// ADR-0043 said could not exist: it recorded an evicted member's push-gw
    /// row as *"never deleted — nobody else can delete it and no client has
    /// 'you were removed' handling"*, so the row went **inert** rather than
    /// away. This is that handling, and it closes the other half of G-36
    /// residual 2 — rotation stops the group notifying the old id; this stops
    /// the ex-member being subscribed to anything at all.
    ///
    /// Unlike the other two callers the retiring id is not a *superseded*
    /// secret but the CURRENT one: nothing supersedes it, because this device
    /// will never receive another rotation for this group. The stored row is
    /// deliberately left in place — the message log and the group registry are
    /// the `Delete this chat` path's business (§6.2.4), and a routing row with
    /// no live subscription notifies nothing.
    ///
    /// Takes the routing chain, like every other read-then-write entry point:
    /// the read of the stored secret and the route-row delete must not
    /// interleave with a rotation that is still in flight from before the
    /// eviction landed.
    ///
    /// Best-effort and never throws. Returns `false` when there was no
    /// readable secret to name an id from — done rather than failed, since a
    /// device that never held one was never subscribed under a `push_gid`.
    @discardableResult
    public func retireOwnSubscription(accountIdB64: String, groupIdHex: String) async -> Bool {
        await withRoutingLock(accountIdB64: accountIdB64, groupIdHex: groupIdHex) {
            [weak self] in
            guard let self,
                  let secret = self.store.getSecret(
                    accountIdB64: accountIdB64, groupIdHex: groupIdHex)
            else { return false }
            await self.retireOldPushGid(oldSecret: secret)
            return true
        }
    }

    /// ADR-0043 — retire a SUPERSEDED `push_gid`: drop this device's push-gw
    /// subscription under it, then delete its reverse-index row.
    ///
    /// Called on BOTH sides of a rotation with the OLD secret — by the
    /// rotator after it re-arms under the new id, and by every remaining
    /// member when the gated receive path applies a CHANGED secret. Both
    /// must, and for the same reason: `POST /push/group/unsubscribe` deletes
    /// exactly the `(device, group)` row named in the request, and a client
    /// knows only its OWN device pseudonym, so nobody can tear down anyone
    /// else's row. If only the rotator unsubscribed, every OTHER member would
    /// leave a live row under the old id — one dead subscription per member
    /// per rotation, which is the "versioned cluster archive" ADR-0043
    /// § Context calls strictly worse than not rotating at all.
    ///
    /// The **subscription** teardown is what carries the privacy value; the
    /// route row is local bookkeeping (a reverse-map miss already degrades to
    /// the generic "New message" banner under the ADR-0041 fail-closed
    /// posture), so it is deleted immediately — the simpler invariant —
    /// rather than retained for an overlap window.
    ///
    /// Best-effort and never throws: the caller has ALREADY switched to the
    /// new id, so a failure here must not unwind the rotation. The cost of a
    /// failed unsubscribe is a stale row that nothing notifies again.
    public func retireOldPushGid(oldSecret: Data) async {
        guard let oldGid = Self.derivePushGid(secret: oldSecret) else {
            NSLog("push-routing old push_gid derivation failed")
            return
        }
        do {
            // BOUNDED: same reason as the subscribe — this is the tail of a
            // rotation, still holding the routing row, and a hung push-gw
            // here would wedge it. A deadline is swallowed exactly as an
            // unreachable push-gw is, at the cost this method already
            // documents: a stale row that nothing notifies again.
            try await withNetworkTimeout("unsubscribe") { [unsubscribeUnderPushGid] in
                try await unsubscribeUnderPushGid(oldGid)
            }
        } catch {
            // The new id is already armed; a failed unsubscribe leaves a row
            // that nothing notifies again, not a broken group.
            NSLog("push_gid unsubscribe failed: \(error)")
        }
        store.removeRoute(pushGidHex: Self.hex(oldGid))
    }

    /// RECEIVE side — a member got the distribution envelope: run the
    /// authority gate, then persist the secret (idempotent) + subscribe
    /// under the derived `push_gid`. Best-effort — never throws into the
    /// poll loop.
    ///
    /// **The authority gate (`lakko-security` MEDIUM-1).** The send side
    /// only ever lets the ADR-0022 OWNER mint
    /// (``ensureProvisioned(accountIdB64:groupIdHex:group:identity:broadcast:)``),
    /// but without a matching receive-side check ANY member could broadcast
    /// a fresh 32-byte envelope and unilaterally rotate the whole group's
    /// routing id — every member would overwrite their secret and
    /// re-subscribe, stranding an orphan push-gw subscription per churn. So
    /// a *changed* secret is accepted only from the owner or an ADR-0022
    /// admin (ADR-0043 option (b)), never a plain member; see
    /// ``classifyIncomingSecret(incoming:stored:senderKey:authority:)`` for
    /// the full matrix.
    ///
    /// **ADR-0043 — the receiving half of a rotation.** When the accepted
    /// secret CHANGES the one this device held, the id it was subscribed
    /// under is now dead, so the old `push_gid` is retired here too
    /// (``retireOldPushGid(oldSecret:)``). Every member tears down its OWN
    /// push-gw row because no client can delete anyone else's; skip this and
    /// each rotation leaves one dead row PER MEMBER. The old secret is read
    /// from the `stored` snapshot taken BEFORE the persist below — the secret
    /// row is overwritten in place, so after the write the old bytes are
    /// unrecoverable and the retiring id could never be named.
    ///
    /// - Parameters:
    ///   - group: the live MLS `Group` — the ONLY authority for both the
    ///     role table (owner + admins) and the sender leaf → signature-key
    ///     mapping. Never trust a value carried in the envelope.
    ///   - senderLeaf: the MLS leaf index that authored the application
    ///     message (surfaced by `ProcessedMessage`, the same
    ///     MLS-authenticated attribution the ADR-0023 author-only gate
    ///     uses). `nil` ⇒ unattributable ⇒ only a byte-identical re-send
    ///     is accepted.
    ///   - mintEpoch: ADR-0044 — the stamp the envelope carried. Clamped
    ///     against this device's own group epoch, then compared against the
    ///     stored pair by the canonical order. A stale re-broadcast carries a
    ///     valid secret from an authorised sender and is refused ONLY by that
    ///     order — which is the whole of G-36 residual 9.
    ///
    /// **ADR-0044 — a refused envelope changes nothing at all.** Both new
    /// refusals return before the persist AND before the teardown, so a stale
    /// replay can never retire the live routing id.
    public func receiveSecret(
        accountIdB64: String,
        groupIdHex: String,
        secret: Data,
        mintEpoch: UInt64,
        group: Group,
        senderLeaf: UInt32?
    ) async {
        await withRoutingLock(accountIdB64: accountIdB64, groupIdHex: groupIdHex) {
            [weak self] in
            guard let self else { return }
            await self.applyReceivedSecret(
                accountIdB64: accountIdB64, groupIdHex: groupIdHex, secret: secret,
                mintEpoch: mintEpoch, group: group, senderLeaf: senderLeaf)
        }
    }

    /// Body of ``receiveSecret(accountIdB64:groupIdHex:secret:mintEpoch:group:senderLeaf:)``,
    /// past the serializing chain lock. Runs alone for its group.
    ///
    /// ## The read-decide-write span MUST NOT gain a suspension point
    ///
    /// From the `getRecord` below through the `persistSecret` further down is
    /// one critical section, and it is currently atomic for TWO independent
    /// reasons — the chain lock this runs inside, and the fact that every
    /// callee in the span is synchronous (``PushRoutingStore`` is
    /// `UserDefaults` + CryptoKit with no async I/O; the `Group` roster / role
    /// / epoch reads are synchronous FFI; `@MainActor` yields only at a
    /// suspension point). **The belt is the lock; the braces are the
    /// synchrony.** Keep both. An `await` introduced into this span — an async
    /// Keychain read, the store moving into the App Group container,
    /// `derivePushGid` gaining an async FFI hop, the store becoming an `actor`
    /// — would put the correctness entirely on the lock, so if the lock is
    /// ever refactored the two changes must be reviewed together.
    ///
    /// The hazard is a **stale read across a concurrent write**, which is NOT
    /// the torn-read hazard the "ONE read for the pair" note below covers. A
    /// torn read pairs a superseded secret with a live epoch out of ONE row;
    /// a stale read is a whole, self-consistent row that another gate has
    /// already replaced. `ChatRepository.receivePushRoutingSecret` spawns a
    /// detached `Task` per envelope, so one poll batch puts several gates in
    /// flight: if this span suspended, both would classify against the same
    /// pre-batch row, the rotation would be applied, and the stale envelope —
    /// byte-identical to that pre-batch row — would then be waved through as
    /// ``SecretVerdict/acceptIdentical``, which short-circuits BOTH the clamp
    /// and the order by design. The superseded secret would be re-persisted
    /// one turn behind the rotation and the device would end on the id the
    /// evicted member still derives: F-A (G-36 residual 9) restored, with no
    /// malice and no crash. Satisfying the torn-read note does not prevent
    /// that; only holding the row across the whole span does.
    private func applyReceivedSecret(
        accountIdB64: String,
        groupIdHex: String,
        secret: Data,
        mintEpoch: UInt64,
        group: Group,
        senderLeaf: UInt32?
    ) async {
        // ONE read for the pair — a torn (secret, epoch) read could rank a
        // superseded secret at a live epoch.
        let record = store.getRecord(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
        let stored = record?.secret
        // `getRecord` returns nil for THREE different reasons: nothing
        // stored, no device wrap key, or the AEAD open failed. Only the
        // first is a provable hole. `hasSecret` is the key-free presence
        // check that separates them, so an admin cannot exploit a transient
        // Keychain / decrypt failure to overwrite a secret we really hold.
        let storedUnreadable = stored == nil
            && store.hasSecret(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
        let resolved = Self.resolveSenderAuthority(group: group, senderLeaf: senderLeaf)
        // ADR-0044 — the clamp ceiling. `nil` (a torn group) fails closed in
        // the decision core, since with no ceiling a stamp cannot be bounded.
        let verdict = Self.classifyIncomingSecret(
            incoming: secret,
            incomingMintEpoch: mintEpoch,
            stored: stored,
            storedMintEpoch: record?.mintEpoch ?? 0,
            storedUnreadable: storedUnreadable,
            ownEpoch: Self.readGroupEpoch(group),
            senderKey: resolved.senderKey,
            authority: resolved.authority
        )
        guard verdict.accepts else {
            // Low-cardinality diagnostics only — never the secret or any
            // signature key. `held` distinguishes the two refusal shapes:
            // an overwrite attempt (held: true) vs a seed from an
            // unauthorised sender (held: false). ADR-0044 open question:
            // neither epoch is secret, and a stale replay is invisible in the
            // field without them.
            NSLog(
                "push-routing secret REFUSED (ADR-0042 role gate / ADR-0044 order): "
                    + "\(verdict.diagnosticTag) "
                    + "(leaf \(senderLeaf.map(String.init) ?? "none"), "
                    + "authority \(resolved.authority.diagnosticTag), "
                    + "held \(stored != nil), unreadable \(storedUnreadable), "
                    + "mintEpoch \(mintEpoch), storedMintEpoch \(record?.mintEpoch ?? 0))"
            )
            return
        }
        // ADR-0043 — did this accepted secret SUPERSEDE one we held? Captured
        // from the pre-persist snapshot, because the row is clobbered in place.
        let superseded: Data? = (stored != nil && stored != secret) ? stored : nil
        // ADR-0044 — an `accept-identical` re-persist must leave the STORED
        // epoch untouched. It is the one accept that clears NEITHER the
        // authority gate nor the future clamp (it short-circuits both,
        // deliberately, so a retry stays idempotent for any sender), so
        // writing the incoming stamp here would be an **epoch-inflation
        // channel open to a plain member**: relay the group's current secret
        // bytes stamped `UInt64.max` and every later rotation — including the
        // owner's — is refused as stale forever. That is exactly the permanent
        // freeze the clamp exists to prevent, reached around it. The write
        // itself is kept (it re-arms the subscription, and is otherwise a
        // content no-op).
        let epochToPersist = verdict == .acceptIdentical
            ? (record?.mintEpoch ?? 0)
            : mintEpoch
        // `distributed: true` — this secret ARRIVED over the group journal,
        // so its distribution envelope is provably on the wire. Since the
        // retry branch of `ensureProvisioned` is open to ANY holder, this flag
        // is what keeps a receiver from re-broadcasting an envelope it just
        // watched travel: a settled row is never retried by anyone.
        //
        // **That same single write is ADR-0044's complementary fix on iOS.**
        // The ordering gate stops a stale secret from SPREADING but does not
        // repair the device still holding one: a device whose broadcast failed
        // sits on a `pending` row and keeps re-emitting a value the group has
        // moved past. `putRouting` is ONE sealed `UserDefaults.set` covering
        // secret + flag + epoch, so applying an accepted secret replaces the
        // bytes AND clears the pending flag atomically — after any accept
        // there is nothing stale left to re-emit. Structural, and pinned by a
        // test rather than left incidental.
        guard persistSecret(
            accountIdB64: accountIdB64, groupIdHex: groupIdHex, secret: secret,
            distributed: true, mintEpoch: epochToPersist) != nil else {
            return
        }
        // Arm the NEW id first, so a failure between the two never leaves this
        // device subscribed to nothing.
        await subscribeUnderPushGidBestEffort(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
        // ADR-0043 — then retire the superseded one. This device is the only
        // party that can drop its own push-gw row.
        if let superseded {
            NSLog(
                "push-routing secret ROTATED by a peer (retiring the old push_gid; "
                    + "authority \(resolved.authority.diagnosticTag))")
            await retireOldPushGid(oldSecret: superseded)
        }
    }

    // MARK: - Receive-side authority gate (lakko-security MEDIUM-1)

    /// Verdict for an inbound push-routing-secret distribution envelope.
    ///
    /// The two `accept*` cases both persist + (re-)subscribe; the three
    /// `reject*` cases drop the envelope entirely — no persist, no
    /// subscribe, and (the one that carries the security value) no
    /// ``retireOldPushGid(oldSecret:)``.
    public enum SecretVerdict: Equatable {
        /// A CHANGED (or first-ever) secret that cleared BOTH the ADR-0042 /
        /// ADR-0043 authority gate and the ADR-0044 order.
        case accept
        /// Byte-identical to the secret already held — an idempotent no-op
        /// re-persist that needs neither authority nor ordering, which is
        /// what keeps the any-holder re-broadcast and the post-add
        /// redistribute working when relayed. Distinct from ``accept``
        /// because the persist MUST leave the stored `mintEpoch` untouched:
        /// this path short-circuits the future clamp, so writing the incoming
        /// stamp would be an epoch-inflation channel open to a plain member.
        case acceptIdentical
        /// An unauthorised writer (a plain member, an unresolvable sender, or
        /// an unreadable role table). The ADR-0042 / ADR-0043 authority
        /// refusal, unchanged.
        case reject
        /// ADR-0044 — the envelope's `mintEpoch` is ahead of this device's
        /// OWN current group epoch, which an honest stamp can never be
        /// (`stamp <= seal epoch <= receiver's epoch at dispatch`). Refused
        /// BEFORE authority is consulted, so not even the owner can stamp
        /// `UInt64.max` and freeze the group's routing id permanently — that
        /// would escalate G-36 residual 3 from a churn an owner rotation
        /// repairs into an unrecoverable freeze. Also covers an unreadable
        /// own epoch: with no ceiling there is nothing to check against.
        case rejectFuture
        /// ADR-0044 — an AUTHORISED envelope that is not strictly newer than
        /// what this device holds under the canonical
        /// `(mintEpoch, secretBytes)` order. The F-A replay lands here: a
        /// device whose broadcast failed re-emits the PRE-rotation secret, an
        /// authorised sender relays it, and without an ordering notion every
        /// member would accept it and retire the live gid — migrating the
        /// group back onto the id an evicted member still derives,
        /// permanently.
        case rejectStale

        /// True when the verdict lets the envelope through.
        public var accepts: Bool {
            switch self {
            case .accept, .acceptIdentical: return true
            case .reject, .rejectFuture, .rejectStale: return false
            }
        }

        /// Low-cardinality tag for the refusal log — never key material.
        var diagnosticTag: String {
            switch self {
            case .accept: return "accept"
            case .acceptIdentical: return "accept-identical"
            case .reject: return "reject-not-authorized"
            case .rejectFuture: return "reject-future"
            case .rejectStale: return "reject-stale"
            }
        }
    }

    /// What the live MLS group says about who is allowed to set the
    /// group's `push_routing_secret`.
    ///
    /// The three states are deliberately kept distinct: a group with NO
    /// role table is not the same as a group whose role table could not be
    /// read (the first is a legacy / open group, the second is a failure
    /// that must fail closed).
    public enum SecretAuthority: Equatable {
        /// The group carries an ADR-0022 role table: the owner's 32-byte
        /// Ed25519 signature public key plus the admin set (each also a
        /// raw 32-byte signature key, as `RoleTable.admins` reports them).
        /// Owner outranks admin outranks member — see
        /// ``classifyIncomingSecret(incoming:stored:senderKey:authority:)``.
        case roleTable(owner: Data, admins: [Data])
        /// The group carries NO role table (a legacy / open group). There
        /// is no owner to gate on, so the pre-gate behaviour is preserved
        /// (accept) rather than bricking push for such groups — matching
        /// the ADR-0042 open-question decision to leave un-owned legacy
        /// groups alone until a natural re-creation.
        case noRoleTable
        /// The role table or the member roster could not be read (the FFI
        /// threw). Fail-closed: a *changed* secret is refused.
        case unreadable

        /// Low-cardinality tag for the refusal log — never the key bytes.
        var diagnosticTag: String {
            switch self {
            case .roleTable: return "role-table"
            case .noRoleTable: return "no-role-table"
            case .unreadable: return "unreadable"
            }
        }
    }

    /// The pure, side-effect-free receive-side decision.
    ///
    /// ## Two questions, in this order: WHO may write, then WHICH write survives
    ///
    /// ADR-0042 / ADR-0043 gave this gate an **authority** notion. ADR-0044
    /// adds the **ordering** notion it never had — the omission that made a
    /// retry of the current secret and a replay of a superseded one
    /// byte-indistinguishable, hence the F-A rotation undo (G-36 residual 9).
    /// Authority still decides who may write; the order arbitrates only
    /// between envelopes that already cleared it.
    ///
    /// The steps, in the order ADR-0044 fixes them:
    ///
    ///   1. **identical bytes → ``SecretVerdict/acceptIdentical``.** No
    ///      authority, no ordering. UNCHANGED, and this is exactly what
    ///      preserves retry semantics. Matched on the SECRET BYTES alone: a
    ///      retry that reached us over a different route is still the same
    ///      value, whatever epoch it claims, and the caller leaves the stored
    ///      pair untouched.
    ///   2. **`incomingMintEpoch > ownEpoch` → ``SecretVerdict/rejectFuture``.**
    ///      Evaluated BEFORE authority, so not even the owner can freeze the
    ///      group's routing id by stamping `UInt64.max`. Sound because an
    ///      honest stamp can never exceed the receiver's own epoch
    ///      (`stamp <= seal <= receiver's epoch at dispatch`), so this never
    ///      refuses an honest envelope.
    ///   3. the ADR-0042 / ADR-0043 **authority** gate — owner / admin /
    ///      no-role-table / unreadable — UNCHANGED, including the rank, the
    ///      owner-evaluated-first branch, the fail-closed refusals, and the
    ///      legacy no-role-table acceptance.
    ///   4. **no stored secret → accept** (the seed path; nothing to order
    ///      against).
    ///   5. **strictly newer under `(mintEpoch, secretBytes)` → accept**,
    ///      otherwise ``SecretVerdict/rejectStale``.
    ///
    /// ## The semantic narrowing, stated plainly
    ///
    /// A total order and unconditional owner-priority cannot both hold. The
    /// owner branch is still decided before anything is read about what is
    /// stored, so the owner still clears AUTHORITY first — but its envelope
    /// now also has to win the order, and **at an exact epoch tie the bytes
    /// decide and the owner can lose to an admin**. That is deliberate: the
    /// property owner-priority existed to provide (arrival-order
    /// independence) is preserved by a strictly stronger mechanism — a total
    /// order is arrival-order independent for every participant, not merely
    /// for the owner's envelope. Under ADR-0043 option (b) one removal yields
    /// N+1 same-epoch mints, so the tie is the common path, not a corner case.
    ///
    /// **Owner outranks admin outranks member**, expressed purely as *who
    /// may overwrite what* — so no authority level is ever persisted
    /// alongside the secret.
    ///
    /// | stored vs incoming | authority + sender | verdict |
    /// |---|---|---|
    /// | identical | any sender, incl. a plain member | **accept** (idempotent re-broadcast) |
    /// | changed / first | `.roleTable`, sender == owner | **accept** (the owner may always rotate) |
    /// | **first, PROVABLY absent** | `.roleTable`, sender ∈ admins | **accept** (an admin SEEDS a hole) |
    /// | **changed**, or absence unprovable | `.roleTable`, sender ∈ admins | **accept** (ADR-0043 — an admin may ROTATE) |
    /// | changed / first | `.roleTable`, plain member (or no sender) | **reject** |
    /// | changed / first | `.noRoleTable` | **accept** (legacy / open group) |
    /// | changed / first | `.unreadable` | **reject** (fail-closed) |
    ///
    /// The admin-seeds row is what keeps delivery working: ADR-0022 lets
    /// owner OR admin add a member, and
    /// ``redistribute(accountIdB64:groupIdHex:broadcast:)`` fires after any
    /// add-member commit, so a member added by a non-owner admin holds no
    /// secret and would otherwise never get push routing (the owner only
    /// broadcasts at mint time).
    ///
    /// **ADR-0043 (ratified option (b)) widened the admin row from a refusal
    /// to an acceptance.** ADR-0022 lets an admin commit the Remove that
    /// makes a rotation necessary, so refusing the overwrite left an admin
    /// able to evict a member but unable to retire that member's routing id.
    /// The `stored` / `storedUnreadable` pair no longer gates an admin's
    /// *authority* — it only distinguishes a provable seed from a rotate for
    /// the diagnostic log. Owner is still evaluated FIRST, so the
    /// arrival-order-independent "owner takes priority" property is
    /// unchanged, and the widening stops at the admin tier: a plain member is
    /// still refused whether or not a secret is held.
    ///
    /// This widens G-36 residual 3 to "a malicious admin can churn the
    /// group's routing id at will" — a wake outage, never a downgrade (a
    /// member without the current secret DEFERS, invariant #4).
    ///
    /// The byte-identical escape hatch is what keeps the owner's
    /// post-add-member `redistribute` idempotent even when it is relayed by
    /// a path whose sender attribution does not resolve — re-persisting the
    /// value already held changes nothing, so it can never be an attack.
    /// - Parameters:
    ///   - stored: the secret this device already holds, `nil` when it
    ///     holds none **or** could not read it.
    ///   - storedUnreadable: set when the local secret could NOT be read
    ///     (no device wrap key, or the AEAD open failed). `stored == nil`
    ///     then means "unknown", NOT "provably absent" — so an admin write is
    ///     a ROTATE rather than a seed. Since ADR-0043 option (b) authorises
    ///     an admin either way this no longer changes the accept/reject
    ///     outcome; it keeps the diagnostic honest about what was observed.
    ///   - incomingMintEpoch: ADR-0044 — the stamp the envelope carried.
    ///   - storedMintEpoch: ADR-0044 — the epoch the STORED secret was minted
    ///     under (`0` for a legacy row, the lowest position in the order).
    ///     Ignored when `stored` is `nil`.
    ///   - ownEpoch: ADR-0044 — this device's OWN current group epoch, the
    ///     clamp ceiling. `nil` when `Group.epoch()` could not be read, which
    ///     FAILS CLOSED: with no ceiling there is nothing to check a stamp
    ///     against.
    public static func classifyIncomingSecret(
        incoming: Data,
        incomingMintEpoch: UInt64,
        stored: Data?,
        storedMintEpoch: UInt64 = 0,
        storedUnreadable: Bool = false,
        ownEpoch: UInt64?,
        senderKey: Data?,
        authority: SecretAuthority
    ) -> SecretVerdict {
        // (1) Idempotent case first — a re-observed identical secret changes
        // nothing, so it needs no authority and no ordering. Safe from anyone
        // (a plain member included), and required for `redistribute`
        // idempotence. Deliberately matched on the SECRET BYTES alone, and
        // the caller must leave the stored pair untouched on this verdict.
        if let stored, stored == incoming { return .acceptIdentical }

        // From here the envelope would CHANGE this device's routing id.
        //
        // (2) Clamp the future BEFORE authority (ADR-0044 § 4). An unbounded
        // ordering rule would hand any authorised writer a permanent freeze —
        // strictly worse than the churn it already has, because a churn
        // self-repairs at the next owner rotation and a freeze does not.
        guard let ownEpoch else { return .rejectFuture }
        if incomingMintEpoch > ownEpoch { return .rejectFuture }

        // (3) Authority — unchanged from ADR-0042 / ADR-0043.
        let authorised: SecretVerdict
        switch authority {
        case .noRoleTable:
            // No ADR-0022 role table ⇒ nobody to check against. Preserve
            // the pre-gate behaviour for legacy / open groups.
            authorised = .accept
        case .unreadable:
            // Could not read the roles / roster ⇒ cannot authorise a
            // CHANGE. Fail-closed (invariant #4).
            return .reject
        case .roleTable(let ownerKey, let admins):
            // An unattributable sender (no leaf, or a leaf that is not in
            // the current roster) can never authorise a set. An EMPTY key
            // is not a valid Ed25519 signature key — refuse it outright so
            // a degenerate role-table entry can never be matched by an
            // equally degenerate resolved sender.
            guard let senderKey, !senderKey.isEmpty else { return .reject }

            if senderKey == ownerKey {
                // OWNER — the priority on AUTHORITY. May always set the
                // secret, whatever is stored and in whatever order the
                // envelopes arrive. Its envelope must still win the ORDER.
                authorised = .accept
            } else if admins.contains(senderKey) {
                // ADMIN — may SEED a hole (so a member the admin added gets
                // routing) AND, under ADR-0043 option (b), ROTATE an
                // established secret (so an admin-committed Remove can retire
                // the evicted member's routing id). `stored` /
                // `storedUnreadable` no longer gate the authority.
                authorised = .accept
            } else {
                // A plain member — the tier boundary the widening did NOT
                // cross.
                return .reject
            }
        }

        // (4) Nothing stored to order against — the seed path.
        guard let stored else { return authorised }

        // (5) The order decides WHICH authorised write survives. Routed
        // through the audited core, never re-derived here.
        return isNewer(
            incomingEpoch: incomingMintEpoch,
            incomingSecret: incoming,
            storedEpoch: storedMintEpoch,
            storedSecret: stored
        ) ? authorised : .rejectStale
    }

    /// Read the ADR-0022 role table (owner + admins) + resolve `senderLeaf`
    /// to its signature public key off the live MLS `Group`.
    ///
    /// The leaf → key mapping comes from `Group.members()` (each
    /// `GroupMember` carries `leafIndex` + `signaturePublicKey`), and the
    /// authorised keys from `Group.groupRoles()` (`RoleTable.owner` +
    /// `RoleTable.admins`) — both read from the group's own authenticated
    /// state, never from the envelope. Any FFI throw folds into
    /// ``SecretAuthority/unreadable`` so the classifier fails closed. A
    /// `senderLeaf` that is not in the current roster resolves to `nil`
    /// (⇒ rejected on a change), not to `.unreadable`.
    public static func resolveSenderAuthority(
        group: Group,
        senderLeaf: UInt32?
    ) -> (authority: SecretAuthority, senderKey: Data?) {
        let authority: SecretAuthority
        do {
            if let table = try group.groupRoles() {
                authority = .roleTable(owner: table.owner, admins: table.admins)
            } else {
                authority = .noRoleTable
            }
        } catch {
            return (.unreadable, nil)
        }

        guard let senderLeaf else { return (authority, nil) }
        do {
            let key = try group.members()
                .first { $0.leafIndex == senderLeaf }?
                .signaturePublicKey
            return (authority, key)
        } catch {
            return (.unreadable, nil)
        }
    }

    /// Re-broadcast the CURRENT secret to a group (used right after an
    /// add-member commit so the new member learns it on their next poll).
    /// No-op when this device does not hold the secret. Idempotent for
    /// existing members (a re-received secret is the same value → an
    /// idempotent re-persist). Best-effort.
    ///
    /// Unlike the mint, this runs against a row the mint already settled, so
    /// BOTH outcomes have to be recorded: a success settles the flag exactly
    /// like the mint-time broadcast (so an owner whose original broadcast
    /// failed stops re-broadcasting once ANY of them lands), and a FAILURE
    /// un-settles it (so the newly-added member this redistribute was for is
    /// not stranded on a permanently `true` flag that no sweep would ever
    /// re-send). See ``broadcastAndSettle(secret:accountIdB64:groupIdHex:what:broadcast:)``.
    ///
    /// **This device does not have to be the owner** — ADR-0022 lets an admin
    /// add a member too, so an admin reaches this path, and the pending flag
    /// it leaves behind is what makes ITS OWN retry fire: the re-broadcast
    /// branch of
    /// ``ensureProvisioned(accountIdB64:groupIdHex:group:identity:broadcast:)``
    /// is open to any secret HOLDER, not just the owner. It has to be — the
    /// owner's row is still settled and the owner never learns this broadcast
    /// failed, so an owner-only retry branch would strand the newly-added
    /// member permanently. Re-sending an identical secret needs no authority
    /// (the receive gate accepts it from anyone); only the mint does.
    ///
    /// **ADR-0044 — this is a RE-BROADCAST of an already-minted secret, so it
    /// replays the STORED `mintEpoch`.** It must not stamp the post-add
    /// epoch: the secret did not change, only its audience did, and
    /// re-stamping would let a routine add-member out-rank a rotation minted
    /// at a higher epoch. Read as a PAIR from one record so the two can never
    /// come from either side of a concurrent receive.
    public func redistribute(
        accountIdB64: String,
        groupIdHex: String,
        broadcast: @escaping (Data) async throws -> Void
    ) async {
        // Serialized with receive / rotate / provision: this reads the pair,
        // then spends a network round-trip in `broadcastAndSettle` before
        // writing the `distributed` flag back. The `matching:` guard already
        // stops that write from describing a secret that changed underneath,
        // but holding the row keeps the re-broadcast from racing a rotation
        // into re-sending a secret the group has just moved past.
        await withRoutingLock(accountIdB64: accountIdB64, groupIdHex: groupIdHex) {
            [weak self] in
            guard let self,
                  let record = self.store.getRecord(
                    accountIdB64: accountIdB64, groupIdHex: groupIdHex) else {
                return
            }
            await self.broadcastAndSettle(
                secret: record.secret, mintEpoch: record.mintEpoch,
                accountIdB64: accountIdB64, groupIdHex: groupIdHex,
                what: "redistribute", broadcast: broadcast)
        }
    }

    /// Compose + broadcast a secret's distribution envelope, then durably
    /// record the outcome: on success the group has been told
    /// (``PushRoutingStore/markDistributed(accountIdB64:groupIdHex:matching:)``),
    /// on failure it has NOT
    /// (``PushRoutingStore/markPending(accountIdB64:groupIdHex:matching:)``).
    ///
    /// This is the whole recovery mechanism: the envelope is never stored
    /// because it is recomputable from the secret, so a failed broadcast
    /// needs to leave behind exactly one bit — "not yet on the wire" — and
    /// ``ensureProvisioned(accountIdB64:groupIdHex:group:identity:broadcast:)``
    /// picks it up on the next launch / chat-open.
    ///
    /// **Why the failure path WRITES rather than leaving the flag alone.**
    /// The flag is per-GROUP, not per-member. On the mint / re-broadcast
    /// paths the row is already pending, so the write is a no-op — but a
    /// ``redistribute(accountIdB64:groupIdHex:broadcast:)`` runs against a row
    /// the mint already settled, for the benefit of the ONE member that was
    /// just added. If that broadcast fails and the flag stays `true`, no
    /// sweep ever re-sends and the new member defers push forever: the same
    /// permanent-no-push bug the flag exists to prevent, scoped to one member
    /// instead of the whole group. Un-settling the row is what puts it back in
    /// **this device's own** retry branch — which is why that branch is open to
    /// any holder and not just the owner: an admin may have been the one to add
    /// the member, and the owner's row would still read settled. `matching:`
    /// keeps a failed OLD-secret broadcast from un-settling a newly rotated
    /// secret that did travel.
    ///
    /// **ADR-0044 — `mintEpoch` is a PARAMETER, never read from a group here.**
    /// Every caller has to say explicitly which epoch it is replaying: a MINT
    /// passes the epoch it just stamped, a RE-BROADCAST passes the STORED
    /// epoch. Sourcing it inside this helper would silently re-stamp on every
    /// retry, which is the one mistake that turns the whole ordering gate into
    /// a no-op.
    ///
    /// **The broadcast is BOUNDED** (``withNetworkTimeout(_:_:)``): it runs
    /// under the serializing routing-row lock, so a hung DS would otherwise
    /// wedge the row forever. A deadline lands in the `catch` below — the
    /// SAME branch a DS error takes — so the row is left `pending` and the
    /// any-holder re-broadcast branch re-sends it. The row is never flagged
    /// distributed on a call that merely stopped answering.
    private func broadcastAndSettle(
        secret: Data,
        mintEpoch: UInt64,
        accountIdB64: String,
        groupIdHex: String,
        what: String,
        broadcast: @escaping (Data) async throws -> Void
    ) async {
        do {
            let envelope = try Self.composeSecretEnvelope(
                mintEpoch: mintEpoch, secret: secret)
            try await withNetworkTimeout("broadcast") {
                try await broadcast(envelope)
            }
            store.markDistributed(
                accountIdB64: accountIdB64, groupIdHex: groupIdHex, matching: secret)
        } catch {
            store.markPending(
                accountIdB64: accountIdB64, groupIdHex: groupIdHex, matching: secret)
            NSLog(
                "push-routing secret \(what) failed (secret marked "
                    + "undistributed; this device re-broadcasts on the next "
                    + "provision): \(error)")
        }
    }

    // MARK: - Owner-mint funnel

    /// Ensure a group's push routing is provisioned for this device.
    ///
    ///   * OWNER (ADR-0022), no secret held → **mint**: persist marked
    ///     undistributed, broadcast the distribution envelope (via
    ///     `broadcast`, which posts an E2EE MLS application message to the
    ///     group journal), and mark distributed once it lands. A non-owner
    ///     holding nothing does neither — it waits for the receive path.
    ///   * ANY HOLDER (owner, admin or plain member), secret held but **not**
    ///     distributed → **re-broadcast**: recompose the envelope from the
    ///     stored secret and try again, marking distributed on success.
    ///   * Then subscribe under `push_gid` if the secret is held (owner
    ///     just minted it; a member may already hold one from a prior
    ///     receive), else defer.
    ///
    /// ## Why the re-broadcast branch exists
    ///
    /// A mint whose broadcast fails (offline, DS error) leaves the secret
    /// durably stored, so a guard shaped `isOwner && !hasSecret` would
    /// short-circuit on every later launch and never retry — the group's
    /// other members never learn the secret and defer push **forever**
    /// (only an unrelated add-member's
    /// ``redistribute(accountIdB64:groupIdHex:broadcast:)`` would
    /// accidentally repair it). The durable `distributed` flag is what makes
    /// that failure recoverable: the envelope itself is never stored because
    /// it is recomputable from the secret, so one bit is the entire state
    /// needed. A re-broadcast is safe by construction — the ADR-0042 receive
    /// gate accepts a byte-identical secret from any sender, so it is an
    /// idempotent no-op re-persist for every member that already has it.
    ///
    /// ## Why only the MINT is owner-gated
    ///
    /// The two branches need different authority, so the guard is split
    /// rather than wrapping both in `isOwner`:
    ///
    ///   * the **mint** CHOOSES a value — 32 random bytes — so two
    ///     independent minters would diverge the group's `push_gid` and break
    ///     routing. Exactly one source may create it: the ADR-0022 owner.
    ///   * the **re-broadcast** re-sends bytes the device already holds. The
    ///     receive gate accepts a byte-identical secret from ANY sender, and
    ///     checks that *before* consulting authority, so this needs none —
    ///     and grants none: a member could always have replayed an identical
    ///     envelope to the same effect.
    ///
    /// Gating the retry on `isOwner` too is what left ADR-0022's admin path
    /// broken: an ADMIN may add a member, so an admin runs
    /// ``redistribute(accountIdB64:groupIdHex:broadcast:)``; when that fails,
    /// the ADMIN's row goes pending while the owner's row stays settled and
    /// the owner never learns anything went wrong. With an owner-only retry
    /// nobody re-sends and the newly-added member gets no push, permanently.
    ///
    /// This single funnel serves BOTH the chat-open path (any member opens a
    /// group) AND the legacy-sweep (rehydrating an un-provisioned group).
    /// A non-owner never MINTS here; it re-broadcasts only a secret it
    /// already holds, and otherwise just subscribes (or defers, when it holds
    /// none, and waits for the receive path).
    ///
    /// A per-group in-flight guard coalesces a concurrent legacy-sweep +
    /// onAppear. The mint → persist is synchronous (no `await` before
    /// ``PushRoutingStore/hasSecret(accountIdB64:groupIdHex:)`` flips true),
    /// so even absent the guard a second concurrent call would never
    /// re-mint; the guard additionally suppresses a redundant re-broadcast.
    ///
    /// - Parameter broadcast: posts the distribution-envelope bytes to the
    ///   group journal as an E2EE MLS application message
    ///   (persist-before-publish). Only invoked on the two write branches
    ///   (the owner-only mint + the any-holder re-broadcast); a device holding
    ///   no secret never reaches it. It MUST throw when the bytes did not reach
    ///   the journal — a silent no-op return would durably mark an
    ///   undelivered secret as distributed.
    public func ensureProvisioned(
        accountIdB64: String,
        groupIdHex: String,
        group: Group,
        identity: Identity,
        broadcast: @escaping (Data) async throws -> Void
    ) async {
        let lockKey = Self.lockKey(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
        // The COALESCING guard, which answers a different question from the
        // serializing chain below: not "may this section see a half-applied
        // row" but "should this redundant trigger run at all". A concurrent
        // legacy-sweep and chat-view onAppear want the identical outcome, so
        // the second is dropped rather than queued behind the first and made
        // to re-do it. Held across the chain wait, which cannot deadlock: a
        // caller that sees the flag returns immediately instead of blocking,
        // and no section inside the chain calls back in here.
        if inflight.contains(lockKey) { return }
        inflight.insert(lockKey)
        defer { inflight.remove(lockKey) }

        await withRoutingLock(accountIdB64: accountIdB64, groupIdHex: groupIdHex) {
            [weak self] in
            guard let self else { return }
            await self.performEnsureProvisioned(
                accountIdB64: accountIdB64, groupIdHex: groupIdHex,
                group: group, identity: identity, broadcast: broadcast)
        }
    }

    /// Body of ``ensureProvisioned(accountIdB64:groupIdHex:group:identity:broadcast:)``,
    /// past the coalescing guard + the serializing chain lock. Runs alone for
    /// its group, so the `hasSecret` / `getRecord` reads below cannot be
    /// invalidated by a concurrent receive or rotation before the mint /
    /// re-broadcast decision they drive is acted on.
    private func performEnsureProvisioned(
        accountIdB64: String,
        groupIdHex: String,
        group: Group,
        identity: Identity,
        broadcast: @escaping (Data) async throws -> Void
    ) async {
        // ADR-0022 role: `.owner`. `nil` = open / legacy group with no role
        // table — nobody mints (avoids divergence), matching the ADR-0042
        // open-question decision to keep un-owned legacy groups on deferred
        // push until a natural re-creation.
        let isOwner = (try? group.ownRole(identity: identity)) == .owner

        if !store.hasSecret(accountIdB64: accountIdB64, groupIdHex: groupIdHex) {
            // MINT — OWNER ONLY. The secret is 32 RANDOM bytes, so a second
            // independent mint would diverge the group's `push_gid`; exactly
            // one source may create it. A non-owner holding nothing simply
            // waits for the distribution envelope (and defers push meanwhile).
            if isOwner {
                // ADR-0044 — the ONE place in this function that reads the
                // group's epoch: a MINT stamps the current one. A failed read
                // fails closed (no mint, hence no push) rather than stamping a
                // guess, since an unordered secret is exactly what the v2
                // envelope exists to eliminate.
                guard let mintEpoch = Self.readGroupEpoch(group) else {
                    NSLog("push-routing mint skipped: group epoch unreadable")
                    await subscribeUnderPushGidBestEffort(
                        accountIdB64: accountIdB64, groupIdHex: groupIdHex)
                    return
                }
                // Persist synchronously (flips `hasSecret` true) before any
                // await, marked NOT distributed, then tell the group over the
                // E2EE journal. Persist-before-broadcast stays: minting twice
                // would diverge the group's routing id, which is far worse
                // than re-broadcasting one envelope.
                guard let secret = Self.mintSecret() else { return }
                if persistSecret(
                    accountIdB64: accountIdB64, groupIdHex: groupIdHex, secret: secret,
                    distributed: false, mintEpoch: mintEpoch) != nil {
                    await broadcastAndSettle(
                        secret: secret, mintEpoch: mintEpoch,
                        accountIdB64: accountIdB64, groupIdHex: groupIdHex,
                        what: "broadcast", broadcast: broadcast)
                }
            }
        } else if let pending = store.getRecord(
            accountIdB64: accountIdB64, groupIdHex: groupIdHex), !pending.distributed {
            // RE-BROADCAST — the repair branch, open to ANY HOLDER (owner,
            // admin or plain member). A broadcast for this secret failed
            // (offline / DS error), so somebody who needs it never learned it:
            // after a failed MINT that is the whole group, after a failed
            // `redistribute(accountIdB64:groupIdHex:broadcast:)` it is the one
            // member that was just added. Either way they defer push forever
            // unless this row is retried.
            //
            // **Why this branch needs no authority.** It re-sends the exact
            // bytes already stored, and the ADR-0042 receive gate accepts a
            // BYTE-IDENTICAL secret from any sender *before* any authority is
            // consulted (see
            // ``classifyIncomingSecret(incoming:stored:senderKey:authority:)``),
            // so every member that already holds it performs a no-op
            // re-persist and the member that does not gets the seed. Owner
            // authority is required for the MINT — where the value is chosen —
            // never for re-sending a value the group already agreed on, so
            // this grants a member no capability it did not already have (it
            // could always replay an identical envelope and be accepted).
            //
            // Gating it on `isOwner` is what left the hole this closes: ADR-0022
            // lets an ADMIN add a member, so an admin runs `redistribute`, and
            // its failure marked the ADMIN's row pending while the owner's row
            // stayed settled — nobody would ever retry.
            //
            // **ADR-0044 — this branch REPLAYS the stored `mintEpoch`
            // verbatim.** It is the exact site the F-A replay travels through,
            // so re-stamping the current epoch here would hand the stale
            // secret a newer position than the rotation that superseded it and
            // reproduce the finding with the ordering gate fully armed. The
            // pair comes from the ONE `getRecord` above.
            await broadcastAndSettle(
                secret: pending.secret, mintEpoch: pending.mintEpoch,
                accountIdB64: accountIdB64, groupIdHex: groupIdHex,
                what: "re-broadcast", broadcast: broadcast)
        }
        // A device holding a secret it cannot READ (`getRecord` nil while
        // `hasSecret` is true — wrap key rotated / corrupt row) falls through
        // deliberately: it must not re-mint (that would diverge the group's
        // routing id) and cannot recompose the envelope from a secret it
        // cannot read.

        // Subscribe (or defer) under push_gid. Owner just minted →
        // subscribes; a member with no secret → deferred (never under
        // group_id).
        await subscribeUnderPushGidBestEffort(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
    }

    // MARK: - ADR-0043: rotate the routing secret when a member is REMOVED
    //
    // ## What rotation buys, stated precisely
    //
    // A removed member keeps the group's `push_routing_secret` on disk, so it
    // can still derive the current `push_gid` — and since nothing tears its
    // subscription down, push-gw keeps waking that device for a group it was
    // evicted from. The leak is not storage hygiene: an ex-member learns
    // "this group was active at time T" on every send, indefinitely.
    //
    // It cannot be closed by unsubscribing, because
    // `POST /push/group/unsubscribe` deletes exactly the `(device, group)`
    // row named in the request and a client knows only its OWN device
    // pseudonym — no remaining member can reach the ex-member's row, and the
    // ex-member's client has no "you were removed" handling to do it itself.
    // Re-keying is the only mechanism the GROUP has: after a rotation nobody
    // ever notifies the old id again, so that row goes **inert without the
    // ex-member's cooperation**.
    //
    // What it does NOT buy: unlinkability of the CLUSTER. The provider sees
    // `{A,B,C,D}` under the old id and `{A,B,C}` under the new one and
    // re-links by token-set intersection — the set difference even names the
    // evicted device. That is a deliberate, recorded trade (ADR-0043 § crux),
    // not an oversight: closing it needs per-device-blinded routing ids
    // (G-36 residual 1).
    //
    // ## Rotate on REMOVAL only, never on add
    //
    // An added member is *supposed* to route —
    // ``redistribute(accountIdB64:groupIdHex:broadcast:)`` already hands it
    // the current secret. Rotating on add would also turn that redistribute
    // into a CHANGED secret against every existing member's stored one on
    // every add, for zero privacy gain.

    /// Verdict of the pure ``membershipRotationDecision(previousMemberKeys:currentMemberKeys:ownRole:)``
    /// core.
    public enum PushRotationDecision: Equatable {
        /// Membership SHRANK and this device may rotate.
        case rotate
        /// Membership grew or is unchanged — an add / role change / ordinary
        /// commit. The routing id stays stable, which is the whole point of
        /// rotating only at removals.
        case skipNoShrink
        /// No usable before/after snapshot (first observation of the group,
        /// or a `members()` read that threw). Never rotate on an unknown
        /// delta — a missing snapshot would otherwise read as "everyone
        /// departed".
        case skipUnknownMembership
        /// A shrink, but this device is a plain member (or the group has no
        /// role table). Rotating is owner-or-admin only.
        case skipNotAuthorized
        /// ADR-0045 UX §6.2.8 item 3 — the departing member is US. This
        /// device's own membership has ended, so it must not rotate on its own
        /// eviction.
        ///
        /// It would otherwise, and that is the FINDING rather than a
        /// precaution: a `Remove` rewrites no `GroupContext`, so an evicted
        /// admin's role table still names it an admin,
        /// ``membershipRotationDecision(previousMemberKeys:currentMemberKeys:ownRole:selfEvicted:)``
        /// sees a member depart and returns `.rotate` — and the rotation then
        /// throws at the broadcast, because the envelope is an MLS application
        /// message and `createApplicationMessage` is refused for an inactive
        /// group. Worse than a no-op: the rotation mints a fresh secret,
        /// subscribes this device to a brand-new `push_gid` nobody will ever
        /// notify, retires the old one, and retries the throwing broadcast on
        /// every chat-open.
        ///
        /// The doc on
        /// ``rotateIfMembershipShrank(accountIdB64:groupIdHex:ownRole:previousMemberKeys:currentMemberKeys:mintEpoch:selfEvicted:broadcast:)``
        /// reasons correctly about the device *performing* a removal (its
        /// baseline is already post-removal, so it sees no shrink) and has no
        /// case at all for the device *receiving its own*.
        case skipSelfEvicted
    }

    /// Outcome of the rotation orchestration — the decision values plus what
    /// the IO half resolved to.
    public enum PushRotationOutcome: Equatable {
        /// Minted, persisted, broadcast, re-subscribed, old id retired.
        case rotated
        /// This device holds no READABLE secret, so there is nothing to
        /// ROTATE. Minting is owner-only and lives in
        /// ``ensureProvisioned(accountIdB64:groupIdHex:group:identity:broadcast:)``;
        /// rotation deliberately never mints a first secret, so an admin can
        /// never become a second minter and diverge the group's `push_gid`.
        ///
        /// Covers the unreadable row too (no device wrap key / AEAD open
        /// failed): the OLD id could never be named from bytes we cannot
        /// read, so rotating would strand it subscribed forever.
        case skipNoSecret
        /// ADR-0044 — `Group.epoch()` could not be read, so the new secret
        /// could not be stamped. Fail-closed: an unstamped secret has no
        /// position in the total order, which is exactly what the v2 envelope
        /// exists to eliminate, so the rotation is abandoned rather than
        /// performed unordered. The old secret stays in place and a later
        /// trigger retries.
        case skipUnknownEpoch
        /// See ``PushRotationDecision/skipNoShrink``.
        case skipNoShrink
        /// See ``PushRotationDecision/skipUnknownMembership``.
        case skipUnknownMembership
        /// See ``PushRotationDecision/skipNotAuthorized``.
        case skipNotAuthorized
        /// See ``PushRotationDecision/skipSelfEvicted``.
        case skipSelfEvicted
    }

    /// Stable per-member identities for the membership diff: the
    /// lowercase-hex Ed25519 signature public keys from `Group.members()`.
    ///
    /// Keys, NOT leaf indices: OpenMLS reuses a vacated leaf for the next
    /// Add, so a remove-then-add pair can leave the index set identical while
    /// the membership genuinely changed. The signature key is stable for a
    /// member's lifetime in the group.
    ///
    /// Returns `nil` when the snapshot cannot be read (a torn group), which
    /// the decision core treats as "unknown", never as "everybody left".
    public nonisolated static func memberSignatureKeys(group: Group) -> [String]? {
        guard let roster = try? group.members() else { return nil }
        return roster.map { hex($0.signaturePublicKey) }
    }

    /// Pure decision core — did the membership SHRINK, and may this device
    /// rotate? No IO, so the whole matrix is directly testable (and is the
    /// verbatim port of the web `membershipRotationDecision`, like the
    /// ADR-0037 `restoreVerdict` gate).
    ///
    /// "Shrank" = at least one member present in the previous snapshot is
    /// absent from the current one. Deliberately not a count comparison: a
    /// poll batch can carry a Remove AND an Add, which leaves the count equal
    /// while a member was evicted — and that member must still lose its
    /// routing id.
    ///
    /// The authority check is owner-OR-admin (ADR-0043 ratified option (b)),
    /// matching who ADR-0022 lets commit the Remove in the first place. A
    /// `nil` role (no role table) does NOT rotate: such a group has no owner,
    /// so nobody ever minted a secret for it and there is nothing to rotate —
    /// and letting every member of an un-owned group rotate would have them
    /// each mint divergent secrets.
    ///
    /// - Parameters:
    ///   - previousMemberKeys: member keys observed BEFORE the commits
    ///     landed, or `nil` if unknown.
    ///   - currentMemberKeys: member keys observed AFTER, or `nil` if unknown.
    ///   - ownRole: this device's ADR-0022 role; `nil` = no role table.
    ///   - selfEvicted: ADR-0045 UX §6.2.8 item 3 — has THIS device's own
    ///     membership ended? Evaluated FIRST, ahead of the membership diff and
    ///     ahead of the authority check, because a device that is out of the
    ///     group has no business rotating whatever its stale role table says —
    ///     and because the answer must not depend on which of three skip
    ///     reasons happened to be checked first. Defaulted so every existing
    ///     caller and every existing test reads exactly as it did; `false`
    ///     means "not evicted", which is the pre-ADR-0045-§6.2 behaviour byte
    ///     for byte.
    public nonisolated static func membershipRotationDecision(
        previousMemberKeys: [String]?,
        currentMemberKeys: [String]?,
        ownRole: Role?,
        selfEvicted: Bool = false
    ) -> PushRotationDecision {
        if selfEvicted { return .skipSelfEvicted }
        guard let before = previousMemberKeys, let after = currentMemberKeys else {
            return .skipUnknownMembership
        }
        // A first observation has no baseline to diff against.
        if before.isEmpty { return .skipUnknownMembership }
        let present = Set(after)
        guard before.contains(where: { !present.contains($0) }) else { return .skipNoShrink }
        guard ownRole == .owner || ownRole == .admin else { return .skipNotAuthorized }
        return .rotate
    }

    /// ADR-0043 — re-mint the group's `push_routing_secret`, redistribute it,
    /// and retire the superseded `push_gid`.
    ///
    /// **The caller MUST have already published the Remove commit** — C-61,
    /// and under ADR-0044 it carries TWO guarantees, not one. The envelope is
    /// an MLS application message: sealed at epoch N (pre-removal) the
    /// evicted member is still a member of that epoch and can OPEN it,
    /// learning the new secret and defeating the whole change; sealed at N+1
    /// it is unreadable to the removed leaf (RFC 9420 forward secrecy).
    /// ADR-0044 adds the second: the new secret is STAMPED with
    /// `Group.epoch()` read via `mintEpoch` below, so publishing the Remove
    /// first is also what makes the rotation strictly out-rank the secret it
    /// replaces. Get the order wrong and the stamp is the pre-removal epoch,
    /// which TIES with a stale secret minted at the same epoch — the tie
    /// falls to the byte comparison and the rotation may simply lose. The
    /// guarantee degrades from "always wins" to "coin flip", silently. Use
    /// ``publishRemoveThenRotate(publishRemove:rotate:onRotationError:)`` at
    /// the call site, which is where that ordering is pinned by a test.
    ///
    /// The steps, in the one order that works:
    ///
    ///   1. read the OLD secret and 2. derive the OLD `push_gid` — **before**
    ///      the store write, because the secret row is keyed
    ///      `(account, group)` and is clobbered in place. Persist first and
    ///      the id being retired can no longer be named: an undeletable
    ///      orphan that keeps receiving wakes, i.e. the exact defect being
    ///      fixed;
    ///   3. read the CURRENT (post-removal) epoch as the mint stamp;
    ///   4. mint the NEW secret and persist it `distributed: false`;
    ///   5. broadcast, marking distributed only once the send lands. That
    ///      flag is the whole durability mechanism, reused verbatim from the
    ///      mint path: a failed rotation broadcast leaves the row PENDING,
    ///      and the re-broadcast branch of
    ///      ``ensureProvisioned(accountIdB64:groupIdHex:group:identity:broadcast:)``
    ///      — open to ANY holder — re-sends the stored (i.e. the new) secret
    ///      on the next chat-open / launch. Without it, a failed rotation
    ///      broadcast strands the whole group: the rotator alone on the new
    ///      id, everyone else on the old one, nobody notifying either;
    ///   6. subscribe under the NEW id, then 7./8. retire the OLD one
    ///      (``retireOldPushGid(oldSecret:)``) — in that order, so the device
    ///      is never unsubscribed from both at once.
    ///
    /// Rotation NEVER mints a first secret: with no secret held this is a
    /// no-op (``PushRotationOutcome/skipNoSecret``). Minting stays owner-only
    /// in ``ensureProvisioned(accountIdB64:groupIdHex:group:identity:broadcast:)``,
    /// so widening rotation to admins does not create a second minter.
    ///
    /// Never throws — push is best-effort and a rotation failure must not
    /// unwind the membership change that triggered it.
    ///
    /// - Parameter mintEpoch: ADR-0044 — a PROVIDER, not a value. It is
    ///   invoked inside ``performRotation(accountIdB64:groupIdHex:mintEpoch:broadcast:)``,
    ///   **after** the serializing queue wait, so a rotation that queued
    ///   behind a predecessor stamps the epoch it actually mints at rather
    ///   than one captured before the wait. A value parameter would be read
    ///   at the call site and could be stale by the time the mint happens.
    ///   `nil` fails closed (``PushRotationOutcome/skipUnknownEpoch``).
    ///   Unlike web, this client's rotation entry point holds no `Group`, so
    ///   the caller closes over its own live handle.
    @discardableResult
    public func rotateSecret(
        accountIdB64: String,
        groupIdHex: String,
        ownRole: Role?,
        mintEpoch: @escaping @MainActor () -> UInt64?,
        broadcast: @escaping (Data) async throws -> Void
    ) async -> PushRotationOutcome {
        guard ownRole == .owner || ownRole == .admin else { return .skipNotAuthorized }
        // SERIALIZE: a second trigger for the same group QUEUES behind the
        // in-flight rotation and then runs its own, in full. Running them
        // concurrently would have both mint off the same old secret and strand
        // the first new push_gid subscribed-but-never-notified; *folding* the
        // second into the first would leave a removal committed mid-flight
        // with no rotation sealed after it at all. Chaining is the only shape
        // that avoids both: the successor reads this rotation's output as its
        // own old secret.
        //
        // The chain is shared with the receive / provision / redistribute
        // paths, so a peer's routing envelope cannot land mid-rotation either.
        return await withRoutingLock(accountIdB64: accountIdB64, groupIdHex: groupIdHex) {
            [weak self] () -> PushRotationOutcome in
            guard let self else { return .skipNoSecret }
            return await self.performRotation(
                accountIdB64: accountIdB64, groupIdHex: groupIdHex,
                mintEpoch: mintEpoch, broadcast: broadcast)
        }
    }

    /// Body of ``rotateSecret(accountIdB64:groupIdHex:ownRole:broadcast:)``,
    /// past the authority gate + the serializing chain lock. Steps 1-8 of the
    /// ADR. Runs alone for its group: the caller guarantees no other rotation
    /// of the same `(account, group)` overlaps it, which is what lets step (1)
    /// read an old secret no concurrent rotation is about to clobber.
    private func performRotation(
        accountIdB64: String,
        groupIdHex: String,
        mintEpoch: @MainActor () -> UInt64?,
        broadcast: @escaping (Data) async throws -> Void
    ) async -> PushRotationOutcome {
        // (1)+(2) — the OLD secret, read BEFORE anything overwrites it. The
        // gid is derived from these bytes at teardown time, in
        // `retireOldPushGid`. An absent OR unreadable row cannot be rotated:
        // the old id could never be named, so a rotation would strand it
        // subscribed forever.
        guard let oldSecret = store.getSecret(
            accountIdB64: accountIdB64, groupIdHex: groupIdHex) else {
            return .skipNoSecret
        }

        // (3) — ADR-0044: stamp the mint with the group's CURRENT epoch, read
        // HERE, after the queue wait, so a rotation queued behind a
        // predecessor still stamps the epoch it actually mints at. C-61 is
        // what makes this stamp meaningful: the caller published the Remove
        // commit first, so the epoch is already the POST-removal one and this
        // mint strictly out-ranks anything the evicted member could have been
        // holding. If C-61 were violated the stamp would be the PRE-removal
        // epoch, tie with the stale secret, and fall to the byte tiebreak — a
        // coin flip instead of a guarantee. Fail-closed on an unreadable epoch.
        guard let stamp = mintEpoch() else { return .skipUnknownEpoch }

        // (4) — mint + persist PENDING. Persist before broadcasting for
        // the same reason the mint path does: two divergent random secrets
        // are worse than two identical envelopes.
        guard let next = Self.mintSecret() else { return .skipNoSecret }
        guard persistSecret(
            accountIdB64: accountIdB64, groupIdHex: groupIdHex, secret: next,
            distributed: false, mintEpoch: stamp) != nil else {
            return .skipNoSecret
        }

        // (5) — redistribute at the POST-removal epoch. Failure leaves the
        // row pending for the existing any-holder retry branch; it must NOT
        // abort the local switch, which is already durable.
        await broadcastAndSettle(
            secret: next, mintEpoch: stamp,
            accountIdB64: accountIdB64, groupIdHex: groupIdHex,
            what: "rotation", broadcast: broadcast)

        // (6) — arm the NEW id first, so a failure between the two never
        // leaves this device subscribed to nothing.
        await subscribeUnderPushGidBestEffort(
            accountIdB64: accountIdB64, groupIdHex: groupIdHex)

        // (7)+(8) — retire the OLD id: unsubscribe this device's row, drop
        // the stale reverse-index entry.
        await retireOldPushGid(oldSecret: oldSecret)
        return .rotated
    }

    /// ADR-0043 — the WIRE-OBSERVED trigger: rotate when this device notices
    /// that the membership shrank, i.e. when SOMEONE ELSE committed the
    /// Remove.
    ///
    /// A Commit surfaces from the poll loop with no membership delta attached
    /// (surfacing removed leaves from `lakko-mls` would be exact, but it is
    /// an invariant-#3 change to the MLS state machine and needs its own
    /// ADR), so the removal is INFERRED from a `members()` snapshot taken
    /// either side of the poll batch. The snapshot is cheap to read, so the
    /// diff is effectively free.
    ///
    /// The device that performed the removal itself does NOT come through
    /// here: it applied its own commit locally before the poll, so the next
    /// poll's baseline is already post-removal and shows no shrink. It
    /// rotates directly instead, via
    /// ``rotateSecret(accountIdB64:groupIdHex:ownRole:broadcast:)``, right
    /// after its Remove commit is published.
    ///
    /// Never throws.
    @discardableResult
    public func rotateIfMembershipShrank(
        accountIdB64: String,
        groupIdHex: String,
        ownRole: Role?,
        previousMemberKeys: [String]?,
        currentMemberKeys: [String]?,
        mintEpoch: @escaping @MainActor () -> UInt64?,
        // ADR-0045 UX §6.2.8 item 3 — read from `Group.isActive()` in the SAME
        // synchronous region the `currentMemberKeys` snapshot was taken in, so
        // the eviction and the shrink it caused are judged on the same commit.
        selfEvicted: Bool = false,
        broadcast: @escaping (Data) async throws -> Void
    ) async -> PushRotationOutcome {
        switch Self.membershipRotationDecision(
            previousMemberKeys: previousMemberKeys,
            currentMemberKeys: currentMemberKeys,
            ownRole: ownRole,
            selfEvicted: selfEvicted
        ) {
        case .skipNoShrink: return .skipNoShrink
        case .skipUnknownMembership: return .skipUnknownMembership
        case .skipNotAuthorized: return .skipNotAuthorized
        case .skipSelfEvicted: return .skipSelfEvicted
        case .rotate:
            return await rotateSecret(
                accountIdB64: accountIdB64, groupIdHex: groupIdHex, ownRole: ownRole,
                mintEpoch: mintEpoch, broadcast: broadcast)
        }
    }

    // MARK: - C-61: the rotation broadcast STRICTLY FOLLOWS the Remove commit

    /// **C-61 — publish the Remove commit, THEN rotate.** The single call
    /// site for this device's own removal path, and the reason that ordering
    /// is testable at all rather than being two adjacent statements anyone
    /// can reorder.
    ///
    /// Both halves of what C-61 buys depend on `publishRemove` having
    /// RESOLVED before `rotate` is even started:
    ///
    ///   * **Forward secrecy (ADR-0043).** The rotation envelope is an MLS
    ///     application message. Sealed before the Remove commit it is sealed
    ///     at the pre-removal epoch, where the evicted member is still a
    ///     member and can open it — learning the very secret the rotation
    ///     exists to deny it.
    ///   * **Recency (ADR-0044).**
    ///     ``rotateSecret(accountIdB64:groupIdHex:ownRole:mintEpoch:broadcast:)``
    ///     stamps the new secret with the epoch its `mintEpoch` provider
    ///     reports at mint time. After the commit that is the post-removal
    ///     epoch, so the rotation strictly out-ranks the secret it replaces
    ///     and a stale re-broadcast of that older secret is refused
    ///     ``SecretVerdict/rejectStale`` by every member. Before the commit it
    ///     is the PRE-removal epoch — the same epoch the stale secret carries
    ///     — so the two TIE, the tie falls to the 32 secret bytes, and the
    ///     rotation wins or loses by a coin flip with no signal either way.
    ///
    /// ADR-0043 required this ordering but enforced it only structurally, by
    /// call-site sequencing, with no test on any client. ADR-0044 promotes it
    /// from a property that protects one guarantee to a precondition of two,
    /// so it is pinned here — where a test can drive both callbacks and
    /// observe which epoch the emitted envelope was stamped at.
    ///
    /// The rotation is deliberately fire-and-forget: push is best-effort and
    /// a rotation failure must never unwind the membership change that
    /// triggered it. "Ordered" means the rotation is not STARTED until the
    /// removal has landed, not that the caller waits for it.
    ///
    /// - Parameters:
    ///   - publishRemove: applies + publishes the Remove commit, advancing
    ///     the local MLS epoch. Its throw propagates — a removal that failed
    ///     must not rotate.
    ///   - rotate: started ONLY after `publishRemove` returned. `nil` skips
    ///     the rotation entirely (no session — nothing to rotate for). It
    ///     never throws: ``rotateSecret(accountIdB64:groupIdHex:ownRole:mintEpoch:broadcast:)``
    ///     reports every failure as an outcome, so there is no error channel
    ///     to plumb back at the removal's caller.
    ///   - onRotationStarted: handed the detached rotation `Task` the instant
    ///     it is spawned. Production ignores it (fire-and-forget); a test uses
    ///     it to await the rotation and read its outcome.
    /// - Returns: whatever `publishRemove` returned, so the caller keeps its
    ///   commit id (echo suppression, notices).
    @discardableResult
    public static func publishRemoveThenRotate<T>(
        publishRemove: () async throws -> T,
        rotate: (@MainActor () async -> PushRotationOutcome)?,
        onRotationStarted: ((Task<PushRotationOutcome, Never>) -> Void)? = nil
    ) async rethrows -> T {
        // The `await` here is the whole mechanism: nothing about the rotation
        // is reachable until the removal has actually landed.
        let result = try await publishRemove()
        if let rotate {
            let task = Task { @MainActor in await rotate() }
            onRotationStarted?(task)
        }
        return result
    }

    // MARK: - Store passthrough

    /// Wipe every push-routing secret + reverse-index entry for the
    /// account. Called from ``ChatRepository/forgetDevice()``.
    public func clear(accountIdB64: String) {
        store.clear(accountIdB64: accountIdB64)
    }

    // MARK: - Helpers

    /// Lowercase-hex a byte buffer (the push-gw pseudonym wire form).
    public nonisolated static func hex(_ bytes: Data) -> String {
        bytes.map { String(format: "%02x", $0) }.joined()
    }
}
