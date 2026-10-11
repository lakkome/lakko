// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// Orchestrates the send / poll sides of the
/// `lakko-contacts::ContactRequest` flow against the live
/// `lakko-svc-discovery` + `lakko-svc-ds` services.
///
/// Mirror of:
///
///   * `apps/web/src/contacts/client.ts` (1.17e5)
///   * `apps/android/.../contacts/ContactsClient.kt` (1.15e5)
///
/// Three operations:
///
///   * ``sendContactRequest(senderSigningSeed:senderAccountId:senderSignaturePubkey:recipientUsername:introMessage:)``
///     — resolve `@username` → `account_id` via
///     ``DiscoveryClient/lookup(username:)`` → fetch identity
///     bundle → derive recipient mailbox pseudonym from their
///     Ed25519 pubkey → compose envelope via UniFFI
///     `composeContactRequest` → publish to recipient's mailbox via
///     ``DsClient/publishMailbox(pseudonym:payload:)``, wrapped
///     with ``EnvelopeKind/contactRequest``.
///
///   * ``sendContactRequestToBundle(senderSigningSeed:senderAccountId:senderSignaturePubkey:recipientAccountId:recipientSignaturePubkey:recipientX25519Pubkey:introMessage:)``
///     — same compose+publish path but skipping the lookup +
///     identityFetch round-trips when the caller already has the
///     recipient's bundle in hand (the invite-link flow in 1.14e8).
///
///   * ``pollStrangerInbox(mySignaturePubkey:myX25519Secret:after:waitSecs:)``
///     — fetch the user's own
///     `IdentityPseudonym.fromIdentity`-derived mailbox via
///     ``DsClient/fetchMailbox(pseudonym:after:limit:waitSecs:)``,
///     parse each envelope's kind byte, decode + verify each
///     ContactRequest, surface a mixed list of
///     ``IncomingMailboxEvent`` entries + a fresh cursor. Welcome
///     envelopes pass through as
///     ``IncomingMailboxEvent/welcome(messageId:welcomeBytes:createdAt:)``
///     for the chat surface (1.14f) to consume.
public final class ContactsClient {

    private let discovery: DiscoveryClient
    private let ds: DsClient
    /// G-4 (ADR-0016): per-account account-pubkey pin store used by
    /// ``verifyBundleAccountKey(accountId:bundle:)`` to detect a
    /// rotated AccountIdentity key and gate it behind a verified
    /// supersession proof (fail-closed, rule #2).
    private let pinStore: PeerPinStore
    /// KT-log S3d: the additive transparency-log inclusion verifier.
    /// Runs AFTER the supersession gate on every identity-fetch path
    /// (see ``checkTransparencyLog(accountId:accountIdB64:servedPubkey:)``).
    /// Defaults to the build-time-configured ``TransparencyVerify``;
    /// when transparency is unconfigured the verifier returns `.off`
    /// and the check is a no-op (fail-open). Injectable for tests.
    private let transparencyVerify: TransparencyVerify

    /// ADR-0065 phase 2 (UX 16.1) — where a confirmed key reset is reported:
    /// `(account id, the pinned key before the reset, the published key the
    /// log confirmed as a reset)`, both keys base64. One process-wide hook
    /// rather than a per-instance one, because a `ContactsClient` is built
    /// wherever an identity is fetched and every one of them must report.
    /// The composition root points it at
    /// ``ContactsRepository/noteKeyReset(accountId:previousKeyB64:pendingKeyB64:)``;
    /// `nil` (a test, a tool) drops the observation.
    public static var keyResetSink: ((_ accountId: Data, _ previousKeyB64: String, _ pendingKeyB64: String) -> Void)?

    /// ADR-0066 section 3.5 — the proven-predecessor map. A key change the
    /// gate accepted BY PROOF records the old key as the proven predecessor
    /// of the new one (the same person; nothing is shown); a change reported
    /// as a reset drops the person's predecessors.
    private let predecessors: ProvenPredecessorStore

    public init(
        discovery: DiscoveryClient = DiscoveryClient(),
        ds: DsClient = DsClient(),
        pinStore: PeerPinStore = PeerPinStore(),
        transparencyVerify: TransparencyVerify = TransparencyVerify(),
        predecessors: ProvenPredecessorStore = ProvenPredecessorStore()
    ) {
        self.discovery = discovery
        self.ds = ds
        self.pinStore = pinStore
        self.transparencyVerify = transparencyVerify
        self.predecessors = predecessors
    }

    // MARK: - G-4 (ADR-0016) supersession verify + KT-log S3d on fetch

    /// Verify a freshly-fetched peer identity bundle before its keys
    /// are used. Two layers, in order:
    ///
    ///   1. **Supersession/pin gate (ADR-0016, G-4)** — the primary,
    ///      fail-closed trust. TOFU-pins on first sight; on a changed
    ///      key REQUIRES + verifies a supersession proof and re-pins;
    ///      throws ``SupersessionRefused`` otherwise. See
    ///      ``verifySupersessionGate(accountId:bundle:)``.
    ///
    ///   2. **Transparency-log inclusion (KT-log S3d)** — the ADDITIVE
    ///      equivocation-detection / gossip layer. Best-effort: it HARD
    ///      FAILS only on a `.rollback` (the log shrank — an
    ///      unambiguous attack, surfaced as ``LogRollbackDetected``);
    ///      every other verdict is recorded on the pin record +
    ///      surfaced but does NOT block (the log is
    ///      eventually-consistent). See
    ///      ``checkTransparencyLog(accountId:accountIdB64:servedPubkey:)``.
    ///
    /// MUST be called after every `discovery.identityFetch` and BEFORE
    /// the bundle's keys are used. The supersession gate runs FIRST so
    /// a pin exists for the log check to record its verdict on; the log
    /// check NEVER weakens the supersession gate — a
    /// ``SupersessionRefused`` from gate 1 propagates before gate 2 runs
    /// at all.
    ///
    /// - Parameter accountId: the peer's raw 16-byte account_id.
    /// - Parameter bundle: the fetched ``IdentityFetchResponse``.
    /// - Throws: ``SupersessionRefused`` on a fail-closed supersession
    ///   branch, ``LogRollbackDetected`` on a transparency-log rollback.
    func verifyBundleAccountKey(
        accountId: Data,
        bundle: IdentityFetchResponse
    ) async throws {
        // G-30 #2 (ADR-0019): snapshot the pin BEFORE gate 1 runs. Gate 1
        // re-pins to the new key on a verified rotation (overwriting the
        // pin), but gate 2's rotation-chain check needs the OLD pinned key
        // + the OLD chain-version floor to anchor the chain. Capturing it
        // here keeps gate 1 untouched.
        let oldPin = pinStore.readPin(accountIdB64: base64Encode(accountId))

        // Gate 1 (primary, fail-closed). A throw here short-circuits
        // gate 2.
        do {
            try verifySupersessionGate(accountId: accountId, bundle: bundle)
        } catch var refused as SupersessionRefused {
            // KT-log S4 — on a fail-closed refusal of an UNPROVEN
            // CHANGED key (the moment the reset-vs-compromise distinction
            // matters most), enrich the error with a BEST-EFFORT
            // transparency lookup of the NEW key's `is_reset` marker.
            // This NEVER weakens the gate: gate 1 already threw, we
            // re-throw it unchanged save for the added context. Any log
            // fault leaves `loggedReset` nil.
            refused.loggedReset = await bestEffortServedKeyReset(
                accountId: accountId,
                bundle: bundle
            )
            // ADR-0065 phase 2 (UX 16.1) — when the log confirms the new key
            // as an owner-initiated RESET, the refusal is also a STATE of the
            // contact: recorded here, at the one place every identity fetch
            // passes, so a plain member who never starts anything with the
            // contact learns of it through the scheduled device-list check
            // too. The gate still threw and still throws; nothing is re-pinned
            // (invariant #6: the acceptance gate stays fail-closed until the
            // person verifies the new safety number).
            if KeyResetDetection.isReset(reason: refused.reason, loggedReset: refused.loggedReset),
               let pin = oldPin {
                // ADR-0066 section 3.5 — a later reset of the same account
                // makes every predecessor, proven or not, a superseded key.
                predecessors.forgetPerson(currentKeyB64: pin.pinnedSignaturePubkeyB64)
                Self.keyResetSink?(accountId, pin.pinnedSignaturePubkeyB64, bundle.signaturePubkey)
            }
            throw refused
        }

        // ADR-0066 section 3.5 — gate 1 passed. When the published key
        // differs from the pin taken BEFORE it ran, the gate accepted the
        // change by a verified supersession proof (its only accepting branch
        // for a changed key) and re-pinned: a PROVEN ROTATION, with or
        // without a key-reset attestation beside it. The old key becomes the
        // account's proven predecessor: the leaves bound to it stay the same
        // person. No flag, no banner, no closed chat, no eviction; the
        // ADR-0065 phase-2 sink above is never reached on this path.
        if let pin = oldPin, pin.pinnedSignaturePubkeyB64 != bundle.signaturePubkey {
            predecessors.record(
                oldKeyB64: pin.pinnedSignaturePubkeyB64,
                newKeyB64: bundle.signaturePubkey,
                evidence: .verifiedProof)
        }

        // Gate 2 (additive, best-effort). Hard-fails only on rollback.
        // G-30 #2 (ADR-0019): pass the OLD pin + the bundle's supersession
        // proof so the log check can route a key ROTATION through the
        // chain verifier (anchoring the transition to the operator-signed
        // log) instead of the standalone single-key inclusion check.
        let accountIdB64 = base64Encode(accountId)
        guard let servedPubkey = base64Decode(bundle.signaturePubkey) else {
            // The supersession gate already accepted this bundle's
            // pubkey (it TOFU-pinned / matched it via base64), so this
            // is unreachable; skip the additive check rather than throw.
            return
        }
        try await checkTransparencyLog(
            accountId: accountId,
            accountIdB64: accountIdB64,
            servedPubkey: servedPubkey,
            oldPin: oldPin,
            supersessionProofB64: bundle.supersessionProof
        )
    }

    /// KT-log S4 — best-effort lookup of the served (refused) key's
    /// `is_reset` marker, used ONLY to enrich a ``SupersessionRefused``.
    ///
    /// Wraps ``TransparencyVerify/lookupServedKeyReset(accountId:servedPubkey:)``
    /// (which never throws) and falls back to `nil` if the served pubkey
    /// is not decodable. Returns `true` / `false` only when the log's
    /// latest entry commits to exactly the served key; `nil` otherwise
    /// (off / unreachable / absent / pubkey-divergence). The enrichment
    /// must never break the already-failing refusal path.
    ///
    /// Mirror of the web `bestEffortServedKeyReset`.
    private func bestEffortServedKeyReset(
        accountId: Data,
        bundle: IdentityFetchResponse
    ) async -> Bool? {
        guard let servedPubkey = base64Decode(bundle.signaturePubkey) else {
            return nil
        }
        return await transparencyVerify.lookupServedKeyReset(
            accountId: accountId,
            servedPubkey: servedPubkey
        )
    }

    /// G-4 (ADR-0016) — verify a freshly-fetched peer identity
    /// bundle's AccountIdentity key against the local pin, re-pinning
    /// (via a supersession proof) when the key rotated. The primary,
    /// fail-closed trust gate; runs FIRST inside
    /// ``verifyBundleAccountKey(accountId:bundle:)``.
    ///
    /// Decision table (ADR-0016 rule #2, fail-closed):
    ///   1. No existing pin → TOFU-pin the fetched key (marker 0).
    ///      Trust-on-first-use is unchanged for a first observation.
    ///   2. Pin matches the fetched `signature_pubkey` → proceed (no
    ///      key change; the common steady-state case).
    ///   3. Pin DIFFERS → REQUIRE `bundle.supersessionProof`:
    ///        a. absent → throw ``SupersessionRefused`` (`.proofAbsent`).
    ///        b. `verifySupersession(proof, pinnedOldKey, now)` throws
    ///           → ``SupersessionRefused`` (`.proofInvalid`).
    ///        c. the verifier's authorised new key !=
    ///           `bundle.signaturePubkey` → ``SupersessionRefused``
    ///           (`.keyMismatch`) — defence in depth, the proof must
    ///           authorise exactly the served key.
    ///        d. re-pin advancing the monotonic marker to the proof's
    ///           expiry; a backward marker → ``SupersessionRefused``
    ///           (`.markerBackward`).
    ///      Only on a clean verify do we proceed with the new key.
    ///
    /// NEVER blind-TOFU a CHANGED key — that is the whole point of
    /// the gate.
    ///
    /// - Parameter accountId: the peer's raw 16-byte account_id (the
    ///   stable pin key, NOT the pubkey — rule #3).
    /// - Parameter bundle: the fetched ``IdentityFetchResponse``.
    /// - Throws: ``SupersessionRefused`` on any fail-closed branch.
    func verifySupersessionGate(
        accountId: Data,
        bundle: IdentityFetchResponse
    ) throws {
        let accountIdB64 = base64Encode(accountId)
        let fetchedPubkeyB64 = bundle.signaturePubkey

        guard let existing = pinStore.readPin(accountIdB64: accountIdB64) else {
            // Case 1 — first-ever observation. TOFU-pin and proceed.
            pinStore.setPin(
                accountIdB64: accountIdB64,
                pinnedSignaturePubkeyB64: fetchedPubkeyB64,
                rotationMarker: 0
            )
            return
        }

        // Case 2 — unchanged key. Nothing to verify.
        if existing.pinnedSignaturePubkeyB64 == fetchedPubkeyB64 {
            return
        }

        // Case 3 — the AccountIdentity key changed. A proof is now
        // mandatory; absence is an immediate fail-closed refusal.
        guard let proofB64 = bundle.supersessionProof, !proofB64.isEmpty else {
            throw SupersessionRefused(
                reason: .proofAbsent,
                accountIdB64: accountIdB64,
                message: "the peer's account key changed and no supersession " +
                    "proof was provided — refusing to trust the new key. " +
                    "Please re-verify this contact out of band."
            )
        }
        guard let proofBytes = base64Decode(proofB64) else {
            throw SupersessionRefused(
                reason: .proofInvalid,
                accountIdB64: accountIdB64,
                message: "the supersession proof was not valid base64 — " +
                    "refusing the new key. Please re-verify out of band."
            )
        }
        guard let pinnedOldPubkey = base64Decode(existing.pinnedSignaturePubkeyB64)
        else {
            // The stored pin pubkey is somehow not decodable — treat
            // as an invalid-proof refusal rather than mis-trusting.
            throw SupersessionRefused(
                reason: .proofInvalid,
                accountIdB64: accountIdB64,
                message: "the pinned old key could not be decoded — " +
                    "refusing the new key. Please re-verify out of band."
            )
        }

        let nowSecs = Int64(Date().timeIntervalSince1970)

        // The verifier returns the authorised NEW pubkey, or THROWS
        // on a malformed / wrong-old-key / expired / out-of-window
        // proof.
        let authorisedNewPubkey: Data
        do {
            authorisedNewPubkey = try verifySupersession(
                proofBytes: proofBytes,
                trustedOldPubkey: pinnedOldPubkey,
                nowUnixSecs: nowSecs
            )
        } catch {
            throw SupersessionRefused(
                reason: .proofInvalid,
                accountIdB64: accountIdB64,
                message: "the peer's account key changed and the " +
                    "supersession proof did not verify against the pinned " +
                    "key (\(error)) — refusing the new key. Please re-verify " +
                    "out of band."
            )
        }

        // Defence in depth — the proof must authorise EXACTLY the key
        // the bundle served. A proof that verifies but names a third
        // key is rejected (a malicious server splicing an unrelated
        // valid proof onto a substituted key).
        if base64Encode(authorisedNewPubkey) != fetchedPubkeyB64 {
            throw SupersessionRefused(
                reason: .keyMismatch,
                accountIdB64: accountIdB64,
                message: "the supersession proof verified but authorised a " +
                    "different key than the bundle served — refusing. Please " +
                    "re-verify out of band."
            )
        }

        // Advance the monotonic marker (ADR-0016 rule #4) to the
        // proof's own `expires_at`. A genuine later rotation carries
        // a strictly-newer `issued_at` (hence a newer `expires_at`
        // for a fixed emit window), so a stale older successor's
        // proof — whose `expires_at` predates the current marker — is
        // refused. Re-observing the SAME transition re-pins at the
        // same marker (`>=`, allowed). We read `expires_at` from the
        // already-verified proof bytes (the verifier confirmed the
        // signature over exactly these fields, so the parse is on
        // trusted data).
        let proofExpiresAt: Int64
        do {
            proofExpiresAt = try Self.readProofExpiresAtSecs(proofBytes)
        } catch {
            throw SupersessionRefused(
                reason: .proofInvalid,
                accountIdB64: accountIdB64,
                message: "could not read the proof's expiry — refusing. " +
                    "Please re-verify out of band."
            )
        }
        let result = pinStore.setPin(
            accountIdB64: accountIdB64,
            pinnedSignaturePubkeyB64: fetchedPubkeyB64,
            rotationMarker: proofExpiresAt
        )
        if result == .rejectedBackward {
            throw SupersessionRefused(
                reason: .markerBackward,
                accountIdB64: accountIdB64,
                message: "the supersession proof would move the rotation " +
                    "marker backwards (an older successor) — refusing. " +
                    "Please re-verify out of band."
            )
        }
    }

    /// KT-log S3d — consult the operator-signed transparency log to
    /// confirm the served identity is in the public append-only log
    /// (equivocation detection on top of the supersession/pin gate).
    ///
    /// Runs AFTER ``verifySupersessionGate(accountId:bundle:)`` has
    /// TOFU-pinned / verified the key, so a pin always exists when we
    /// record the verdict. Best-effort POSTURE (ADR-0016 stays the
    /// primary, fail-closed trust):
    ///
    ///   * `.rollback`  → HARD FAIL. Throw ``LogRollbackDetected``. The
    ///     log shrank — an unambiguous attack. This is the ONLY blocking
    ///     verdict for S3d.
    ///   * `.mismatch`  → record + prominent `NSLog`, but DO NOT block
    ///     (S3d). Hard-blocking on mismatch is the natural fail-closed
    ///     refinement and is flagged for `lakko-security` to rule on.
    ///   * `.absent` / `.unreachable` / `.off` → record + an `NSLog`
    ///     (for absent/unreachable) and proceed. The log is
    ///     eventually-consistent; the supersession/pin gate already
    ///     authorised the key.
    ///
    /// The recorded status travels on the account's pin record so the
    /// UI can surface it. Mirror of the web `checkTransparencyLog`.
    ///
    /// G-30 #2 (ADR-0019): when the served key is a ROTATION away from
    /// the pinned one AND the bundle carried a supersession proof, the
    /// check routes through
    /// ``TransparencyVerify/verifyRotationChain(accountId:servedPubkey:ctx:)``
    /// (the chain verifier — root sig + no-rollback + Merkle inclusion +
    /// the supersession proof from the OLD pinned key + a strictly-newer
    /// logged version + freshness) instead of the standalone single-key
    /// ``TransparencyVerify/verifyIdentityInLog(accountId:publishedSignaturePubkey:)``.
    /// The POSTURE is unchanged: only `.rollback` hard-fails; a chain
    /// contradiction surfaces as `.mismatch` (NOT a block); a
    /// not-logged / unreachable / off rotation falls back to the
    /// standalone gate-1 result (no over-block). On a `.confirmed` chain
    /// the proven log version is persisted as the next rotation's
    /// downgrade floor.
    ///
    /// - Parameter accountId: the peer's raw 16-byte account_id.
    /// - Parameter accountIdB64: its base64 form (the pin-store key).
    /// - Parameter servedPubkey: the 32-byte Ed25519 key discovery
    ///   served.
    /// - Parameter oldPin: the pin AS IT WAS BEFORE gate 1 re-pinned
    ///   (G-30 #2) — `nil` on a first-ever observation.
    /// - Parameter supersessionProofB64: the bundle's
    ///   `supersession_proof` (base64), when present — the chain's
    ///   old-key authorisation.
    /// - Throws: ``LogRollbackDetected`` ONLY on a `.rollback` verdict.
    func checkTransparencyLog(
        accountId: Data,
        accountIdB64: String,
        servedPubkey: Data,
        oldPin: PeerPinStore.AccountPubkeyPin? = nil,
        supersessionProofB64: String? = nil
    ) async throws {
        // G-30 #2 — a ROTATION is: a pin existed, its key DIFFERS from
        // the served key, and the bundle carried a supersession proof.
        // Only then can the chain verifier anchor the transition. Anything
        // else (first observation / unchanged key / no proof) keeps the
        // existing single-key inclusion check unchanged.
        let status: LogVerifyStatus
        let isReset: Bool?
        let version: UInt64?
        if let oldPin = oldPin,
           oldPin.pinnedSignaturePubkeyB64 != base64Encode(servedPubkey),
           let proofB64 = supersessionProofB64, !proofB64.isEmpty,
           let proofBytes = base64Decode(proofB64),
           let pinnedOldPubkey = base64Decode(oldPin.pinnedSignaturePubkeyB64) {
            let result = await transparencyVerify.verifyRotationChain(
                accountId: accountId,
                servedPubkey: servedPubkey,
                ctx: RotationContext(
                    supersessionProof: proofBytes,
                    pinnedOldPubkey: pinnedOldPubkey,
                    pinnedLogVersion: oldPin.pinnedLogVersion ?? 0
                )
            )
            status = result.status
            isReset = result.isReset
            version = result.version
        } else {
            let result = await transparencyVerify.verifyIdentityInLog(
                accountId: accountId,
                publishedSignaturePubkey: servedPubkey
            )
            status = result.status
            isReset = result.isReset
            version = nil
        }

        // Rollback is the one hard failure (unambiguous attack).
        if status == .rollback {
            throw LogRollbackDetected(accountIdB64: accountIdB64)
        }

        // Record the verdict on the pin record (best-effort; the pin
        // exists because the supersession gate ran first). KT-log S4
        // also persists the `/latest` entry's `is_reset` marker next to
        // the status (a `nil` clears any stale marker — no latest was
        // read).
        pinStore.recordLogStatus(accountIdB64: accountIdB64, logStatus: status.rawValue)
        pinStore.recordLogReset(accountIdB64: accountIdB64, isReset: isReset)

        // G-30 #2 — persist the chain-confirmed log version ONLY on a
        // `.confirmed` rotation chain (where `version != nil`). A
        // transient `unreachable` / `mismatch` / `absent` verdict must
        // NOT clear a previously-confirmed floor, so we never pass `nil`
        // here.
        if let version = version {
            pinStore.recordLogVersion(accountIdB64: accountIdB64, version: version)
        }

        switch status {
        case .mismatch:
            // Surfaced-but-not-blocked for S3d. The natural fail-closed
            // refinement (hard-block on mismatch) is deferred to a
            // lakko-security ruling — see the method doc + the S3d
            // report.
            NSLog(
                "ContactsClient: transparency-log MISMATCH for account %@ — " +
                "the log's latest key differs from what discovery served " +
                "(or the inclusion proof failed). Proceeding for S3d " +
                "(supersession/pin gate is primary); flag for review.",
                accountIdB64
            )
        case .absent, .unreachable:
            NSLog(
                "ContactsClient: transparency-log check inconclusive (%@) " +
                "for account %@ — proceeding; the log is " +
                "eventually-consistent and the pin gate is primary.",
                status.rawValue, accountIdB64
            )
        case .confirmed, .off, .rollback:
            // `confirmed` / `off` are quiet (the happy path / disabled);
            // `rollback` already returned above.
            break
        }
    }

    /// Fixed serialized length of a supersession proof — mirrors
    /// `lakko_transparency::supersession::SUPERSESSION_PROOF_LEN`
    /// (`version(1) ‖ old(32) ‖ new(32) ‖ issued(8) ‖ expires(8) ‖
    /// sig(64)` = 145).
    private static let supersessionProofLen = 1 + 32 + 32 + 8 + 8 + 64
    /// Byte offset of the big-endian i64 `expires_at` field within a
    /// supersession proof: after version(1) + old(32) + new(32) +
    /// issued(8) = 73.
    private static let supersessionExpiresAtOffset = 1 + 32 + 32 + 8

    /// Read the big-endian i64 `expires_at_unix_secs` field out of a
    /// supersession proof's wire bytes (G-4). Used ONLY after
    /// `verifySupersession` has authenticated the proof, so the parse
    /// runs on trusted, fixed-length data — we still bound-check the
    /// length defensively. Mirror of the web `readProofExpiresAtSecs`.
    static func readProofExpiresAtSecs(_ proofBytes: Data) throws -> Int64 {
        guard proofBytes.count >= supersessionProofLen else {
            // Should be unreachable — the verifier already accepted
            // these bytes — but never index past the buffer.
            throw ContactsError.simple(
                "supersession proof too short (\(proofBytes.count) bytes)"
            )
        }
        // Eight big-endian bytes at the expires_at offset → i64.
        let start = proofBytes.startIndex + supersessionExpiresAtOffset
        var value: Int64 = 0
        for i in 0..<8 {
            value = (value << 8) | Int64(proofBytes[start + i])
        }
        return value
    }

    // MARK: - Resolve

    /// Resolve `@username -> (accountId, signaturePubkey)` through the same
    /// G-4-verified path ``sendContactRequest`` uses: discovery `lookup` ->
    /// `identityFetch` -> ``verifyBundleAccountKey`` (the ADR-0016 pin-store
    /// gate — TOFU on first sight, verified re-pin on rotation, fail-closed
    /// `SupersessionRefused` otherwise) -> extract the signature key.
    ///
    /// Used by the group-chat add-member flow, which then hands the resolved
    /// pair to ``ChatRepository/addMemberToGroup(groupIdHex:recipientAccountId:recipientSignaturePubkey:)``.
    /// The verify must NOT be skipped — adding a member to a group seals a
    /// Welcome under the resolved key, exactly like a 1×1 ContactRequest.
    public func resolveVerifiedRecipient(
        username: String
    ) async throws -> (accountId: Data, signaturePubkey: Data) {
        let lookup: LookupResult
        do {
            lookup = try await discovery.lookup(username: username)
        } catch {
            throw ContactsError.wrap(error)
        }
        guard let recipientAccountId = lookup.account else {
            throw ContactsError.usernameUnknown(lookup.normalisedUsername)
        }

        let bundle: IdentityFetchResponse?
        do {
            bundle = try await discovery.identityFetch(accountId: recipientAccountId)
        } catch {
            throw ContactsError.wrap(error)
        }
        guard let bundle = bundle else {
            throw ContactsError.noIdentityBundle(recipientAccountId)
        }

        try await verifyBundleAccountKey(accountId: recipientAccountId, bundle: bundle)

        guard let recipientSignaturePub = base64Decode(bundle.signaturePubkey) else {
            throw ContactsError.wrap(
                ContactsError.simple("malformed identity bundle on the wire")
            )
        }
        return (recipientAccountId, recipientSignaturePub)
    }

    /// ADR-0060 — the identity bundle of an account a search found, through
    /// the same G-4 pin gate every other identity fetch takes.
    public func resolveVerifiedBundle(
        accountId: Data
    ) async throws -> (signaturePubkey: Data, x25519Pubkey: Data) {
        let bundle: IdentityFetchResponse?
        do {
            bundle = try await discovery.identityFetch(accountId: accountId)
        } catch {
            throw ContactsError.wrap(error)
        }
        guard let bundle else { throw ContactsError.noIdentityBundle(accountId) }
        try await verifyBundleAccountKey(accountId: accountId, bundle: bundle)
        guard let sig = base64Decode(bundle.signaturePubkey),
              let x = base64Decode(bundle.x25519Pubkey) else {
            throw ContactsError.wrap(ContactsError.simple("malformed identity bundle on the wire"))
        }
        return (sig, x)
    }

    /// ADR-0066 section 3.5a rule 1 — who a recipient's key is now, from the
    /// proven-predecessor map this client's gate writes. (This account's own
    /// keys are never the recipient of a request or an ack.)
    func mailboxResolver() -> AccountKeyResolver {
        AccountKeyResolver(proven: predecessors.load())
    }

    // MARK: - Send

    /// Send a `ContactRequest` envelope to `@recipientUsername`,
    /// sealed under the recipient's published X25519 key.
    public func sendContactRequest(
        senderSigningSeed: Data,
        senderAccountId: Data,
        senderSignaturePubkey: Data,
        recipientUsername: String,
        profile: FfiProfile? = nil,
        introMessage: Data,
        reach: ContactRequestReach = .none
    ) async throws -> SendResult {
        let lookup: LookupResult
        do {
            lookup = try await discovery.lookup(username: recipientUsername)
        } catch {
            throw ContactsError.wrap(error)
        }
        guard let recipientAccountId = lookup.account else {
            throw ContactsError.usernameUnknown(lookup.normalisedUsername)
        }

        let bundle: IdentityFetchResponse?
        do {
            bundle = try await discovery.identityFetch(accountId: recipientAccountId)
        } catch {
            throw ContactsError.wrap(error)
        }
        guard let bundle = bundle else {
            throw ContactsError.noIdentityBundle(recipientAccountId)
        }

        // G-4 (ADR-0016) — gate the freshly-fetched AccountIdentity
        // key through the pin store BEFORE we use it to seal a
        // ContactRequest: TOFU on first sight, verified re-pin on a
        // key change, fail-closed (`SupersessionRefused`) on a
        // changed key with no valid proof. Throws on any fail-closed
        // branch — we never send to an unverified rotated key. The
        // additive KT-log S3d inclusion check rides along inside (it
        // hard-fails only on a transparency-log rollback).
        try await verifyBundleAccountKey(accountId: recipientAccountId, bundle: bundle)

        guard let recipientSignaturePub = base64Decode(bundle.signaturePubkey),
              let recipientX25519Pub = base64Decode(bundle.x25519Pubkey) else {
            throw ContactsError.wrap(
                ContactsError.simple("malformed identity bundle on the wire")
            )
        }

        let published = try await composeAndPublishContactRequest(
            senderSigningSeed: senderSigningSeed,
            senderAccountId: senderAccountId,
            senderSignaturePubkey: senderSignaturePubkey,
            recipientSignaturePubkey: recipientSignaturePub,
            recipientX25519Pubkey: recipientX25519Pub,
            profile: profile,
            introMessage: introMessage,
            replyGrant: try await reach.replyGrant(recipientAccountId),
            writeAuth: reach.writeAuth(recipientSignaturePub)
        )

        return SendResult(
            recipientUsername: lookup.normalisedUsername,
            recipientAccountId: recipientAccountId,
            messageId: published.messageId,
            createdAt: published.createdAt
        )
    }

    /// Send a `ContactRequest` directly against a known recipient
    /// bundle — **without** going through the discovery
    /// `lookup → identityFetch` round-trip.
    ///
    /// The invite-link flow (1.14e8) is the canonical caller: the
    /// `InviteCapability` already carries `ownerAccountId`,
    /// `ownerSignaturePubkey`, and `ownerX25519Pubkey`, so the
    /// joiner has everything needed to seal an envelope without
    /// touching `services/discovery`.
    public func sendContactRequestToBundle(
        senderSigningSeed: Data,
        senderAccountId: Data,
        senderSignaturePubkey: Data,
        recipientAccountId: Data,
        recipientSignaturePubkey: Data,
        recipientX25519Pubkey: Data,
        profile: FfiProfile? = nil,
        introMessage: Data,
        reach: ContactRequestReach = .none
    ) async throws -> SendResult {
        // ADR-0066 section 3.5a rule 1 — resolve before addressing. A bundle
        // carried by a link or a search result may name a key its owner has
        // since rotated away BY PROOF: the request then goes to the current
        // key's mailbox, sealed to the current X25519 key, which only the
        // verified directory bundle carries. A failed fetch refuses the send;
        // the old mailbox is never the fallback.
        var recipientSignaturePubkey = recipientSignaturePubkey
        var recipientX25519Pubkey = recipientX25519Pubkey
        if mailboxResolver().current(of: recipientSignaturePubkey) != recipientSignaturePubkey {
            let current = try await resolveVerifiedBundle(accountId: recipientAccountId)
            recipientSignaturePubkey = current.signaturePubkey
            recipientX25519Pubkey = current.x25519Pubkey
        }
        let published = try await composeAndPublishContactRequest(
            senderSigningSeed: senderSigningSeed,
            senderAccountId: senderAccountId,
            senderSignaturePubkey: senderSignaturePubkey,
            recipientSignaturePubkey: recipientSignaturePubkey,
            recipientX25519Pubkey: recipientX25519Pubkey,
            profile: profile,
            introMessage: introMessage,
            replyGrant: try await reach.replyGrant(recipientAccountId),
            writeAuth: reach.writeAuth(recipientSignaturePubkey)
        )
        return SendResult(
            recipientUsername: "",
            recipientAccountId: recipientAccountId,
            messageId: published.messageId,
            createdAt: published.createdAt
        )
    }

    /// Shared compose-and-publish path used by both send surfaces.
    /// Mirror of the Kotlin private method — pulled out so the two
    /// public flows don't drift on subtle details (mailbox-pseudonym
    /// derivation, envelope-kind wrapping, sender-pseudonym choice).
    private func composeAndPublishContactRequest(
        senderSigningSeed: Data,
        senderAccountId: Data,
        senderSignaturePubkey: Data,
        recipientSignaturePubkey: Data,
        recipientX25519Pubkey: Data,
        profile: FfiProfile?,
        introMessage: Data,
        replyGrant: FfiReachGrant?,
        writeAuth: DsMailboxWriteAuth?
    ) async throws -> PublishedRequest {
        let recipientPseudonym: Data
        let senderPseudonym: Data
        do {
            // Both callers hand in the recipient's CURRENT key (a bundle the
            // gate just verified, or one re-resolved above), so the mailbox
            // of the key the envelope is sealed for is the current mailbox.
            recipientPseudonym = try IdentityPseudonym
                .fromIdentity(pubkey: recipientSignaturePubkey).asBytes()
            senderPseudonym = try IdentityPseudonym
                .fromIdentity(pubkey: senderSignaturePubkey).asBytes()
        } catch {
            throw ContactsError.wrap(error)
        }

        let validAfterMs = UInt64(Date().timeIntervalSince1970 * 1000)

        // 1.9b2: Privacy Pass / App-Attest attestation blob. The
        // iOS dev path ships an empty token — wiring the real
        // App Attest round-trip (DCAppAttestService) belongs in
        // the 1.20+ Privacy Pass sub-sprint, gated on Apple's
        // dev-account provisioning. The empty value lands the
        // request in the DS ingest layer's "no attestation tier"
        // rate-limit bucket (lowest ceiling).
        let privacyPassToken = Data()
        // ADR-0048 §2 — the sender's own profile rides inside the sealed
        // envelope, so the recipient can see who is asking before accepting.
        // The profile is optional to the request: one the core refuses to
        // compose (a stored name that no longer passes the bounds) is left out
        // rather than blocking the request itself.
        let compose: (FfiProfile?) throws -> Data = { profile in
            try composeContactRequest(
                senderSigningSeed: senderSigningSeed,
                senderAccountId: senderAccountId,
                recipientX25519Pub: recipientX25519Pubkey,
                fromPseudonym: senderPseudonym,
                validAfterMs: validAfterMs,
                privacyPassToken: privacyPassToken,
                profile: profile,
                // ADR-0060 section 6 — the requester's single-use `reply`
                // grant: the recipient writes the accept and the ack with it.
                replyGrant: replyGrant,
                introMessage: introMessage
            )
        }
        let wireBytes: Data
        do {
            do {
                wireBytes = try compose(profile)
            } catch let refused as FfiError where profile != nil && refused.isProfileFieldRefusal {
                NSLog("ADR-0048 contact request sent without a profile: the profile was refused")
                wireBytes = try compose(nil)
            }
        } catch {
            throw ContactsError.wrap(error)
        }

        let envelope = wrapEnvelope(kind: .contactRequest, body: wireBytes)
        let publish: PublishResult
        do {
            // ADR-0060 section 2 — signed with the grant that lets this
            // device reach the recipient (a link grant, a search result's
            // grant, a co-member's); unsigned only for an Everyone mailbox.
            publish = try await ds.publishMailbox(
                pseudonym: recipientPseudonym,
                payload: envelope,
                writeAuth: writeAuth
            )
        } catch {
            if case .http(401, _, _)? = error as? DsClientError {
                throw ContactsError.requestRefused
            }
            throw ContactsError.wrap(error)
        }
        return PublishedRequest(messageId: publish.id, createdAt: publish.createdAt)
    }

    // MARK: - ContactAck

    /// Send a `ContactAck` envelope back to the original sender's
    /// stranger inbox (Phase 1.9b1).
    ///
    /// Called from the recipient's "Accept" handler after they
    /// have decided to accept an incoming ``IncomingContactRequest``.
    /// The ack is independent of MLS group creation — splitting
    /// the two surfaces lets the original sender's UI flip from
    /// "pending" to "accepted" without waiting on the joiner's
    /// `startGroupWith` to complete.
    ///
    /// **Routing:** the ack body carries
    /// `ackToPseudonym = original.fromPseudonym`. The publish
    /// target is the sender's stranger-inbox mailbox — derived
    /// from `original.fromPubkey` via
    /// `IdentityPseudonym.fromIdentity`.
    public func sendContactAck(
        accepterSigningSeed: Data,
        accepterAccountId: Data,
        accepterX25519Pubkey: Data,
        originalRequest: IncomingContactRequest,
        writeAuth: DsMailboxWriteAuth? = nil
    ) async throws -> PublishedAck {
        let senderMailbox: Data
        do {
            // ADR-0066 section 3.5a rule 1 — the requester may have rotated
            // its keys by proof since it asked: the ack goes to the mailbox
            // of its current key.
            senderMailbox = try IdentityPseudonym
                .fromIdentity(pubkey: mailboxResolver().current(of: originalRequest.fromPubkey))
                .asBytes()
        } catch {
            throw ContactsError.wrap(error)
        }

        let acceptedAtMs = UInt64(Date().timeIntervalSince1970 * 1000)

        let wireBytes: Data
        do {
            wireBytes = try composeContactAck(
                accepterSigningSeed: accepterSigningSeed,
                accepterAccountId: accepterAccountId,
                accepterX25519Pub: accepterX25519Pubkey,
                ackToPseudonym: originalRequest.fromPseudonym,
                acceptedAtMs: acceptedAtMs
            )
        } catch {
            throw ContactsError.wrap(error)
        }

        let envelope = wrapEnvelope(kind: .contactAck, body: wireBytes)
        let publish: PublishResult
        do {
            // ADR-0060 section 6 — written with the requester's `reply` grant.
            publish = try await ds.publishMailbox(
                pseudonym: senderMailbox,
                payload: envelope,
                writeAuth: writeAuth
            )
        } catch {
            throw ContactsError.wrap(error)
        }
        return PublishedAck(messageId: publish.id, createdAt: publish.createdAt)
    }

    // MARK: - Poll

    /// Poll the user's stranger-inbox mailbox for new
    /// `ContactRequest`s + Welcome blobs.
    ///
    /// Malformed entries on the wire are **dropped silently** —
    /// the verifier rejects bad signatures, decoding errors are
    /// skipped (the cursor still advances, so we won't re-process
    /// the same row).
    public func pollStrangerInbox(
        mySignaturePubkey: Data,
        myX25519Secret: Data,
        after: Int64 = 0,
        waitSecs: Int = 0,
        mailboxSigner: DsRequestSigner? = nil
    ) async throws -> PollResult {
        let myPseudonym: Data
        do {
            myPseudonym = try IdentityPseudonym
                .fromIdentity(pubkey: mySignaturePubkey).asBytes()
        } catch {
            throw ContactsError.wrap(error)
        }

        let raw: FetchResult
        do {
            // ADR-0053 point 1 — reading a claimed mailbox needs a proof under
            // the identity key it is named after.
            // ADR-0060 section 2 — an unclaimed mailbox answers a read `401`
            // too, so a refusal claims it (claim-on-open) and retries once.
            let ds = self.ds
            raw = try await ds.fetchMailbox(
                pseudonym: myPseudonym,
                after: after,
                waitSecs: waitSecs,
                auth: mailboxSigner.map { signer in
                    DsAuth(signer: signer, reclaim: {
                        guard let proof = signer as? DsProofSigner else { return nil }
                        do {
                            try await ds.claimMailbox(
                                pseudonym: myPseudonym, bodyJson: proof.mailboxClaimBodyJson())
                        } catch {
                            return nil
                        }
                        return signer
                    })
                }
            )
        } catch {
            throw ContactsError.wrap(error)
        }

        var events: [IncomingMailboxEvent] = []
        var cursor = after
        for message in raw.messages {
            cursor = message.id
            switch parseEnvelope(message.payload) {
            case .unknown:
                // Future / unknown kind byte — cursor advances so
                // we don't re-process. No logger plumbed yet.
                continue
            case .welcome(let body):
                events.append(
                    .welcome(
                        messageId: message.id,
                        welcomeBytes: body,
                        createdAt: message.createdAt
                    )
                )
            case .contactAck(let body):
                // 1.9b1: signature is verified inside the UniFFI
                // surface; bad-signature / malformed-wire entries
                // drop silently and the cursor still advances.
                let parsed: ContactAck
                do {
                    parsed = try ContactAck.decodeAndVerify(wire: body)
                } catch {
                    continue
                }
                let ack = IncomingContactAck(
                    messageId: message.id,
                    accepterPubkey: parsed.accepterPubkey(),
                    accepterAccountId: parsed.accepterAccountId(),
                    accepterX25519Pubkey: parsed.accepterX25519Pubkey(),
                    ackToPseudonym: parsed.ackToPseudonym(),
                    acceptedAtMs: parsed.acceptedAtMs(),
                    createdAt: message.createdAt
                )
                events.append(.contactAck(ack))
            case .contactRequest(let body):
                let parsed: ContactRequest
                do {
                    parsed = try ContactRequest.decodeAndVerify(wire: body)
                } catch {
                    // Bad signature, malformed wire bytes, or some
                    // other verifier failure. Drop and move on —
                    // the cursor advances so we won't re-process
                    // the same row.
                    continue
                }

                let intro: String?
                let profile: ProfileClaim?
                var replyGrant: FfiReachGrant?
                do {
                    let opened = try parsed.openEnvelope(
                        recipientX25519Priv: myX25519Secret
                    )
                    intro = String(data: opened.introMessage, encoding: .utf8) ?? ""
                    replyGrant = opened.replyGrant
                    // ADR-0048 — `nil` when the request carried no profile, or
                    // one that failed its bounds (the request itself stays
                    // valid). Bound by the caller to `fromPubkey`, which the
                    // request signature authenticates.
                    profile = opened.profile.map {
                        ProfileClaim(
                            displayName: $0.displayName,
                            username: $0.username,
                            updatedAtMs: $0.updatedAtMs,
                            // Envelope v2 — the requester's account id, or
                            // `nil` when it sent none (ADR-0021 tile match).
                            accountId: $0.accountId,
                            usernameCertificate: $0.usernameCertificate
                        )
                    }
                } catch {
                    intro = nil
                    profile = nil
                }

                let req = IncomingContactRequest(
                    messageId: message.id,
                    fromPubkey: parsed.fromPubkey(),
                    fromAccountId: parsed.fromAccountId(),
                    fromPseudonym: parsed.fromPseudonym(),
                    validAfterMs: parsed.validAfterMs(),
                    intro: intro,
                    profile: profile,
                    createdAt: message.createdAt
                )
                events.append(.contactRequest(req, replyGrant: replyGrant))
            }
        }
        return PollResult(events: events, cursor: cursor, hasMore: raw.hasMore)
    }
}

// MARK: - ADR-0060 request reach

/// How an outgoing contact request is authorised (ADR-0060 section 6): the
/// requester's fresh `reply` grant (registered by the caller before it is
/// returned) and the grant that lets this device write to the recipient.
public struct ContactRequestReach {
    /// Mint the `reply` grant for a request to the account id given.
    public var replyGrant: (Data) async throws -> FfiReachGrant?
    /// The authorisation to write to the recipient with account key given.
    public var writeAuth: (Data) -> DsMailboxWriteAuth?

    public init(
        replyGrant: @escaping (Data) async throws -> FfiReachGrant?,
        writeAuth: @escaping (Data) -> DsMailboxWriteAuth?
    ) {
        self.replyGrant = replyGrant
        self.writeAuth = writeAuth
    }

    /// No reply grant and an unsigned write (tests, and an Everyone mailbox).
    public static let none = ContactRequestReach(replyGrant: { _ in nil }, writeAuth: { _ in nil })
}

// MARK: - Result types

private struct PublishedRequest {
    let messageId: Int64
    let createdAt: String
}

/// Result of ``ContactsClient/sendContactRequest(...)`` and
/// ``ContactsClient/sendContactRequestToBundle(...)``.
public struct SendResult: Equatable {
    /// Normalised form of the username that was resolved, or `""`
    /// if the caller used `sendContactRequestToBundle` (no
    /// discovery round-trip happened).
    public let recipientUsername: String
    /// Raw 16-byte recipient account_id.
    public let recipientAccountId: Data
    /// DS-assigned monotonic id for the published envelope.
    public let messageId: Int64
    /// ISO-8601 timestamp from the DS.
    public let createdAt: String

    public init(
        recipientUsername: String,
        recipientAccountId: Data,
        messageId: Int64,
        createdAt: String
    ) {
        self.recipientUsername = recipientUsername
        self.recipientAccountId = recipientAccountId
        self.messageId = messageId
        self.createdAt = createdAt
    }
}

/// One verified incoming ContactRequest.
public struct IncomingContactRequest: Equatable, Codable {
    public let messageId: Int64
    /// Sender's Ed25519 identity public key (32 bytes). The
    /// signature on the request was verified against this.
    public let fromPubkey: Data
    /// Sender's account_id (16-byte UUIDv7). `nil` for legacy v1
    /// envelopes — the Accept-driven group creation path requires
    /// this and the UI surfaces a "legacy request" message when
    /// it's missing.
    public let fromAccountId: Data?
    /// Sender's per-recipient pseudonym (32 bytes).
    public let fromPseudonym: Data
    public let validAfterMs: UInt64
    /// UTF-8-decoded intro message, or `nil` when there was no
    /// intro / it didn't decrypt. Empty string is a valid payload.
    public let intro: String?
    /// ADR-0048 — the sender's profile as they state it, or `nil` when the
    /// request carried none (or one that failed its bounds). A claim bound to
    /// ``fromPubkey``; its username is shown only once verified.
    public let profile: ProfileClaim?
    public let createdAt: String

    public init(
        messageId: Int64,
        fromPubkey: Data,
        fromAccountId: Data?,
        fromPseudonym: Data,
        validAfterMs: UInt64,
        intro: String?,
        profile: ProfileClaim? = nil,
        createdAt: String
    ) {
        self.messageId = messageId
        self.fromPubkey = fromPubkey
        self.fromAccountId = fromAccountId
        self.fromPseudonym = fromPseudonym
        self.validAfterMs = validAfterMs
        self.intro = intro
        self.profile = profile
        self.createdAt = createdAt
    }
}

/// One verified incoming ``ContactAck`` (Phase 1.9b1).
///
/// The accept-reply that closes the `sendContactRequest`
/// handshake from the original sender's perspective. The
/// Ed25519 signature on the ack has already been verified
/// against ``accepterPubkey``; the caller still has to match
/// ``ackToPseudonym`` against the pending-sent table to pair
/// it with the original request the user fired off.
public struct IncomingContactAck: Equatable {
    public let messageId: Int64
    /// Accepter's Ed25519 identity public key (32 bytes). The
    /// signature on the ack was verified against this.
    public let accepterPubkey: Data
    /// Accepter's `account_id` (16-byte UUIDv7).
    public let accepterAccountId: Data
    /// Accepter's X25519 HPKE public key (32 bytes). Stored
    /// alongside the accepter's pubkey in the local address
    /// book so future replies can be sealed without a fresh
    /// discovery round-trip.
    public let accepterX25519Pubkey: Data
    /// The original request's `from_pseudonym` field — the
    /// receiver matches this against their own identity
    /// pseudonym (and the pending-sent table) to confirm the
    /// ack is for them.
    public let ackToPseudonym: Data
    /// When the accepter pressed Accept (ms since epoch).
    /// Sender-attested; not authoritative — the DS
    /// ``createdAt`` is the trust anchor.
    public let acceptedAtMs: UInt64
    public let createdAt: String

    public init(
        messageId: Int64,
        accepterPubkey: Data,
        accepterAccountId: Data,
        accepterX25519Pubkey: Data,
        ackToPseudonym: Data,
        acceptedAtMs: UInt64,
        createdAt: String
    ) {
        self.messageId = messageId
        self.accepterPubkey = accepterPubkey
        self.accepterAccountId = accepterAccountId
        self.accepterX25519Pubkey = accepterX25519Pubkey
        self.ackToPseudonym = ackToPseudonym
        self.acceptedAtMs = acceptedAtMs
        self.createdAt = createdAt
    }
}

/// Receipt returned by ``ContactsClient/sendContactAck(...)``.
public struct PublishedAck: Equatable {
    public let messageId: Int64
    public let createdAt: String

    public init(messageId: Int64, createdAt: String) {
        self.messageId = messageId
        self.createdAt = createdAt
    }
}

/// Discriminated mailbox event surfaced by
/// ``ContactsClient/pollStrangerInbox(...)``.
public enum IncomingMailboxEvent: Equatable {
    /// A verified request, with the requester's `reply` grant (ADR-0060
    /// section 6) — SECRET, handed to ``ReachCenter`` at receipt and never
    /// persisted with the pending request.
    case contactRequest(IncomingContactRequest, replyGrant: FfiReachGrant? = nil)
    case welcome(messageId: Int64, welcomeBytes: Data, createdAt: String)
    /// 1.9b1 — original recipient's accept-reply.
    case contactAck(IncomingContactAck)
}

/// Result of ``ContactsClient/pollStrangerInbox(...)``.
public struct PollResult: Equatable {
    public let events: [IncomingMailboxEvent]
    /// Fresh cursor for the next poll.
    public let cursor: Int64
    /// `true` if the DS reported more rows past the last returned
    /// id — caller should poll again with the new cursor.
    public let hasMore: Bool

    public init(events: [IncomingMailboxEvent], cursor: Int64, hasMore: Bool) {
        self.events = events
        self.cursor = cursor
        self.hasMore = hasMore
    }
}

// MARK: - Errors

/// Single error type carrying a discriminated cause variant so the
/// UI can branch on the kind without `as?`-walking the underlying
/// client error trees.
public enum ContactsError: Error {
    /// `@<normalisedUsername>` is not registered.
    case usernameUnknown(String)
    /// The recipient's identity bundle is missing from the
    /// discovery service. They've claimed the username but haven't
    /// uploaded a bundle — typically a stale account or a client
    /// mid-rollout.
    case noIdentityBundle(Data)
    /// Any other failure — wraps a ``DiscoveryClientError`` /
    /// ``DsClientError`` / UniFFI `FfiError`. The UI surfaces the
    /// wrapped message verbatim.
    case wrapped(Error)
    /// ADR-0060 — the recipient's mailbox refused the request's key: they
    /// changed who can find them after this device found them.
    case requestRefused

    public static func wrap(_ error: Error) -> ContactsError {
        if let ce = error as? ContactsError { return ce }
        return .wrapped(error)
    }

    /// Helper that wraps a free-form string as a `wrapped`
    /// variant. Used for "malformed identity bundle on the wire"
    /// and similar non-Error string failures.
    public static func simple(_ message: String) -> Error {
        struct Simple: LocalizedError {
            let message: String
            var errorDescription: String? { message }
        }
        return Simple(message: message)
    }
}

extension ContactsError: LocalizedError {
    public var errorDescription: String? {
        switch self {
        case .usernameUnknown(let u):
            return "username @\(u) is not registered"
        case .noIdentityBundle:
            return "no identity bundle for the given account id"
        case .wrapped(let cause):
            return (cause as? LocalizedError)?.errorDescription
                ?? String(describing: cause)
        case .requestRefused:
            return Localization.text("search.people.request_refused")
        }
    }
}

// MARK: - G-4 (ADR-0016) supersession refusal

/// G-4 (ADR-0016) — thrown when a peer's fetched discovery identity
/// bundle carries an AccountIdentity `signature_pubkey` that DIFFERS
/// from the locally-pinned key AND the change cannot be authorised by
/// a valid supersession proof.
///
/// This is the **fail-closed** path (ADR-0016 rule #2): we refuse to
/// send a ContactRequest / start a call to the unverified new key.
/// The UI is expected to catch this and prompt the user to re-verify
/// the peer **out of band** (compare safety numbers, re-scan a QR,
/// etc.) before proceeding. We never blind-TOFU a CHANGED key.
///
/// `reason` discriminates the failure mode for diagnostics — the UX
/// is the same OOB-reverify prompt regardless.
public struct SupersessionRefused: Error, LocalizedError, Equatable {

    /// Why the supersession check refused the changed key. Mirror of
    /// the web `SupersessionRefusedReason` union.
    public enum Reason: String, Equatable {
        /// The bundle carried no `supersession_proof` at all (or it
        /// was not valid base64 / could not be set up to verify).
        case proofAbsent
        /// `verifySupersession` threw — malformed / wrong-old-key /
        /// expired / out-of-range window.
        case proofInvalid
        /// The proof verified but authorised a DIFFERENT key than the
        /// one the bundle actually served (defence in depth).
        case keyMismatch
        /// The marker would move backwards — a within-window rollback
        /// to an older successor (rule #4).
        case markerBackward
    }

    /// The discriminated failure mode.
    public let reason: Reason
    /// base64 of the account_id whose key change was refused.
    public let accountIdB64: String
    /// Human-readable detail for logs / the OOB-reverify prompt.
    public let message: String

    /// KT-log S4 — best-effort transparency-log context for the NEW
    /// (refused) key, attached so the UI can distinguish an
    /// owner-initiated identity RESET from an unexplained key change.
    ///
    ///   * `true`  — the log's latest entry commits to EXACTLY the
    ///     served new key AND records it as an owner-initiated reset
    ///     (ADR-0017 lost-everything fresh-start).
    ///   * `false` — the log's latest entry commits to the served new
    ///     key but does NOT record a reset (an unexplained change →
    ///     possible compromise).
    ///   * `nil`   — the log is off / unreachable / the account is
    ///     absent / the log's latest pubkey does not match the served
    ///     key (the marker is then about a different key, so it is
    ///     unknown).
    ///
    /// This is **best-effort enrichment only**: a log failure NEVER
    /// changes the refusal — gate 1 (the supersession/pin gate) throws
    /// regardless. It only colours the OOB-reverify prompt. Mutable so
    /// the async `verifyBundleAccountKey` can populate it on the
    /// already-thrown error before re-throwing (mirror of the web
    /// `loggedReset` field).
    public var loggedReset: Bool?

    public init(
        reason: Reason,
        accountIdB64: String,
        message: String,
        loggedReset: Bool? = nil
    ) {
        self.reason = reason
        self.accountIdB64 = accountIdB64
        self.message = message
        self.loggedReset = loggedReset
    }

    public var errorDescription: String? { message }

    /// The refusal inside `error`, directly or behind the contacts wrapper.
    public static func find(in error: Error) -> SupersessionRefused? {
        if let refused = error as? SupersessionRefused { return refused }
        if case .wrapped(let cause)? = error as? ContactsError { return find(in: cause) }
        return nil
    }
}

// MARK: - KT-log S3d transparency-log rollback

/// KT-log S3d — thrown when the transparency log's signed root reports
/// a `log_size` SMALLER than the highest this client has ever seen. An
/// append-only log cannot shrink, so a rollback is an unambiguous attack
/// (a server trying to hide a leaf it later appended). This is the ONE
/// transparency-log verdict that hard-fails the identity-fetch path; all
/// other verdicts (`absent` / `unreachable` / `off` / `mismatch`) are
/// recorded + surfaced but do NOT block, because the log is
/// eventually-consistent and the supersession/pin gate is the primary
/// trust (see ``ContactsClient/verifyBundleAccountKey(accountId:bundle:)``).
///
/// Mirror of the web `LogRollbackDetected`.
public struct LogRollbackDetected: Error, LocalizedError, Equatable {
    /// base64 of the account_id whose fetch tripped the rollback.
    public let accountIdB64: String

    public init(accountIdB64: String) {
        self.accountIdB64 = accountIdB64
    }

    public var errorDescription: String? {
        "the transparency log appears to have shrunk (a smaller log_size " +
        "than previously observed) — an append-only log cannot do this, " +
        "so this is an unambiguous rollback attack. Refusing to proceed."
    }
}

extension FfiError {
    /// `true` for the ADR-0048 profile-field refusals — a display name or a
    /// username out of bounds — as opposed to anything about the request.
    var isProfileFieldRefusal: Bool {
        switch self {
        case .UsernameEmpty, .UsernameTooShort, .UsernameTooLong, .UsernameInvalidCharacters,
            .DisplayNameLineBreak, .DisplayNameControlOrFormatCharacter,
            .DisplayNameTooLong, .DisplayNameTooManyBytes:
            return true
        default:
            return false
        }
    }
}
