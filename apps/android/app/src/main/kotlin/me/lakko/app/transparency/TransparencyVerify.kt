// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.transparency

import uniffi.lakko_ffi_uniffi.FfiException
import uniffi.lakko_ffi_uniffi.transparencyAccountKey
import uniffi.lakko_ffi_uniffi.verifyLeafGenesis
import uniffi.lakko_ffi_uniffi.verifyLogInclusion
import uniffi.lakko_ffi_uniffi.verifySupersessionChain

/**
 * Transparency-log inclusion verification (KT-log S3e) — the Android
 * mirror of the web `transparency/verify.ts`. The equivocation-detection
 * / gossip layer on top of the ADR-0016 supersession/pin gate.
 *
 * [verifyIdentityInLog] consults the operator-signed, append-only
 * transparency log to confirm that the identity discovery just served for
 * a contact is the SAME one the public log commits to. A divergence (the
 * log holds a different key, or the log shrank) is surfaced as a
 * [LogVerifyStatus] so the caller can act.
 *
 * State machine ([LogVerifyStatus]):
 *
 *   * [LogVerifyStatus.OFF]         — the integration is unconfigured (no
 *                                     URL or no pinned operator key). The
 *                                     whole check is skipped.
 *   * [LogVerifyStatus.ROLLBACK]    — the signed root's `log_size` is
 *                                     SMALLER than the highest we have
 *                                     seen. An append-only log cannot
 *                                     shrink → an unambiguous attack. HARD.
 *   * [LogVerifyStatus.ABSENT]      — `/latest` 404s: the account is not
 *                                     yet in the log. Benign under
 *                                     eventual consistency OR an off-log
 *                                     equivocation — surfaced, not
 *                                     auto-trusted.
 *   * [LogVerifyStatus.MISMATCH]    — the log's latest entry pubkey
 *                                     differs from what discovery served,
 *                                     OR the inclusion proof failed to
 *                                     verify. Suspicious.
 *   * [LogVerifyStatus.UNREACHABLE] — any network / parse error fetching
 *                                     root/latest/proof.
 *   * [LogVerifyStatus.CONFIRMED]   — root is operator-signed (pinned
 *                                     key), no rollback, and the served
 *                                     pubkey is proven included at the
 *                                     latest version.
 *
 * [verifyIdentityInLog] is BEST-EFFORT and never throws on a network/log
 * fault: it maps faults to a status. The CALLER decides the posture (see
 * [me.lakko.app.contacts.ContactsClient]) — for S3e, only
 * [LogVerifyStatus.ROLLBACK] is a hard failure.
 */
public enum class LogVerifyStatus {
    CONFIRMED,
    ABSENT,
    ROLLBACK,
    MISMATCH,
    UNREACHABLE,
    OFF,
}

/**
 * The outcome of [verifyIdentityInLog] (KT-log S4).
 *
 * Adds the `is_reset` marker the log serves on its `/latest` entry to the
 * bare [status]. [isReset] lets a contact DISTINGUISH an owner-initiated
 * identity RESET (the lost-everything fresh-identity mint — logged with
 * `is_reset=true`, ADR-0017) from an unexplained key change (a possible
 * compromise — `is_reset` false/absent).
 *
 * [isReset] is the `/latest` entry's marker WHENEVER a latest entry was
 * observed — i.e. on [LogVerifyStatus.CONFIRMED] AND on
 * [LogVerifyStatus.MISMATCH] (both imply a latest exists). It is `null`
 * when there was no latest entry to read it from:
 * [LogVerifyStatus.ABSENT] (account not yet logged),
 * [LogVerifyStatus.ROLLBACK] (short-circuited before the latest lookup),
 * [LogVerifyStatus.UNREACHABLE] (network/log fault), and
 * [LogVerifyStatus.OFF] (the integration is unconfigured).
 */
public data class LogVerifyResult(
    /** The S3e state-machine verdict. */
    val status: LogVerifyStatus,
    /** The `/latest` entry's `is_reset` marker when a latest entry was
     *  read ([LogVerifyStatus.CONFIRMED] / [LogVerifyStatus.MISMATCH]);
     *  `null` otherwise. */
    val isReset: Boolean?,
)

/**
 * Inclusion-gate seam — abstracts the native `verifyLogInclusion` call so
 * the status-mapping decision logic in [verifyIdentityInLog] is
 * unit-testable on the host JVM without `liblakko_ffi_uniffi.so`.
 *
 * The production implementation ([NativeInclusionGate]) calls the real
 * UniFFI `verifyLogInclusion`, which THROWS [FfiException] on an
 * inclusion/root-signature failure and RETURNS on success. Tests inject a
 * stub that throws (to simulate failure) or returns (to simulate success)
 * — exactly that contract — so the `CONFIRMED` / proof-fail `MISMATCH`
 * branches can be exercised without building a valid Merkle proof (which
 * needs the Rust log builder, not exposed to the FFI).
 */
public fun interface InclusionGate {
    /**
     * Verify that the entry `(usernameHash, servedPubkey, version)` is
     * included at [LatestEntry.idx] under the operator-signed [root].
     * MUST throw to signal a verification failure; returning normally
     * means the proof verified.
     */
    public fun verify(
        operatorKey: ByteArray,
        root: SignedRoot,
        usernameHash: ByteArray,
        servedPubkey: ByteArray,
        proof: InclusionProof,
        version: ULong,
    )
}

/** Production [InclusionGate] — the real native `verifyLogInclusion`. */
public object NativeInclusionGate : InclusionGate {
    override fun verify(
        operatorKey: ByteArray,
        root: SignedRoot,
        usernameHash: ByteArray,
        servedPubkey: ByteArray,
        proof: InclusionProof,
        version: ULong,
    ) {
        // Throws FfiException on any verification failure (bad root
        // signature, tampered proof, wrong leaf/key/version, length
        // mismatch). The caller maps a throw to MISMATCH.
        verifyLogInclusion(
            operatorPubkey = operatorKey,
            treeRoot = root.treeRoot,
            logSize = root.logSize,
            timestampUnix = root.timestampUnix,
            rootSignature = root.signature,
            leafIndex = proof.idx,
            auditPath = proof.auditPath,
            entryUsernameHash = usernameHash,
            entryIdentityPubkey = servedPubkey,
            entryVersion = version,
        )
    }
}

/**
 * Genesis-self-sig-gate seam (G-30 #2, ADR-0019) — abstracts the native
 * `verifyLeafGenesis` call so the first-observation genesis decision in
 * [verifyIdentityInLog] is unit-testable on the host JVM without the native
 * lib + a real Ed25519 self-sig. The production [NativeGenesisGate] calls the
 * real FFI, which THROWS [FfiException] on a failed check; a test double can
 * THROW (fail) or RETURN (pass) — exactly that contract.
 */
public fun interface GenesisGate {
    /**
     * Verify that [sig] is a valid genesis self-signature by [servedPubkey]
     * over the `(usernameHash, servedPubkey)` binding. MUST throw to signal a
     * failure; returning normally means the self-sig verified.
     */
    public fun verify(sig: ByteArray, usernameHash: ByteArray, servedPubkey: ByteArray)
}

/** Production [GenesisGate] — the real native `verifyLeafGenesis`. */
public object NativeGenesisGate : GenesisGate {
    override fun verify(sig: ByteArray, usernameHash: ByteArray, servedPubkey: ByteArray) {
        // Throws FfiException on a failed check; the caller maps it to MISMATCH.
        verifyLeafGenesis(sig, usernameHash, servedPubkey)
    }
}

/**
 * Rotation-chain-gate seam (G-30 #2, ADR-0019) — abstracts the native
 * `verifySupersessionChain` call so the status-mapping decision logic in
 * [verifyRotationChain] is unit-testable on the host JVM without
 * `liblakko_ffi_uniffi.so`.
 *
 * The production implementation ([NativeChainGate]) calls the real UniFFI
 * `verifySupersessionChain`, which THROWS [FfiException] on ANY chain-gate
 * sub-failure (root sig / no-rollback / inclusion / supersession proof from
 * the old key / non-newer version / freshness) and RETURNS the authorised
 * new pubkey on success. Tests inject a stub that throws (to simulate a
 * chain contradiction) or returns (to simulate a passing chain) — exactly
 * that contract — so the `CONFIRMED` / chain-fail `MISMATCH` branches can be
 * exercised without building a real Merkle proof + a real supersession proof
 * (both need the Rust log builder, not exposed to the FFI).
 */
public fun interface ChainGate {
    /**
     * Verify the rotation transition old→new is anchored to the
     * operator-signed log: the served key is the log's head for the account
     * AND the supersession chain holds. MUST throw to signal a chain-gate
     * failure; returning the authorised new pubkey means the chain verified.
     */
    public fun verify(
        operatorKey: ByteArray,
        root: SignedRoot,
        usernameHash: ByteArray,
        servedPubkey: ByteArray,
        version: ULong,
        proof: InclusionProof,
        supersessionProof: ByteArray,
        pinnedOldPubkey: ByteArray,
        pinnedLogVersion: ULong,
        lastSeenLogSize: ULong,
        nowUnixSecs: Long,
    ): ByteArray
}

/** Production [ChainGate] — the real native `verifySupersessionChain`. */
public object NativeChainGate : ChainGate {
    override fun verify(
        operatorKey: ByteArray,
        root: SignedRoot,
        usernameHash: ByteArray,
        servedPubkey: ByteArray,
        version: ULong,
        proof: InclusionProof,
        supersessionProof: ByteArray,
        pinnedOldPubkey: ByteArray,
        pinnedLogVersion: ULong,
        lastSeenLogSize: ULong,
        nowUnixSecs: Long,
    ): ByteArray =
        // Throws FfiException on any chain-gate sub-failure (bad root
        // signature, rollback, tampered inclusion proof, invalid / wrong-key /
        // expired supersession proof, non-newer logged version). Returns the
        // authorised new pubkey on success. The caller maps a throw to
        // MISMATCH.
        verifySupersessionChain(
            operatorPubkey = operatorKey,
            newTreeRoot = root.treeRoot,
            newLogSize = root.logSize,
            newTimestampUnix = root.timestampUnix,
            newRootSignature = root.signature,
            entryUsernameHash = usernameHash,
            entryIdentityPubkey = servedPubkey,
            entryVersion = version,
            inclusionLeafIndex = proof.idx,
            inclusionAuditPath = proof.auditPath,
            supersessionProof = supersessionProof,
            pinnedOldPubkey = pinnedOldPubkey,
            pinnedVersion = pinnedLogVersion,
            lastSeenLogSize = lastSeenLogSize,
            nowUnixSecs = nowUnixSecs,
        )
}

/**
 * Derive the transparency-log account-key (`username_hash`) for a raw
 * 16-byte `account_id`, via the audited Rust core. Abstracted behind a
 * seam ([accountKeyOf] below) only so the host-JVM status-mapping test can
 * avoid the native lib; production uses the real
 * [uniffi.lakko_ffi_uniffi.transparencyAccountKey].
 */
public fun interface AccountKeyDeriver {
    public fun derive(accountId: ByteArray): ByteArray
}

/** Production [AccountKeyDeriver] — the real native
 *  `transparencyAccountKey` (BLAKE3 over a domain separator + account_id;
 *  the same derivation discovery uses on append, so it cannot drift). */
public object NativeAccountKeyDeriver : AccountKeyDeriver {
    override fun derive(accountId: ByteArray): ByteArray =
        transparencyAccountKey(accountId)
}

/**
 * Verify that [publishedSignaturePubkey] (the AccountIdentity Ed25519 key
 * discovery served for [accountId]) is committed by the operator-signed
 * transparency log.
 *
 * @param accountId                the peer's raw 16-byte account_id.
 * @param publishedSignaturePubkey the 32-byte Ed25519 key discovery
 *                                 returned for that account.
 * @param client                   pre-built client, or `null` to mean
 *                                 "integration off". When omitted,
 *                                 [TransparencyClient.forOptionalConfig]
 *                                 is used.
 * @param operatorKey              the pinned operator verifying key (32
 *                                 bytes), or `null` for "off". When
 *                                 omitted, [TransparencyConfig.pinnedOperatorKey]
 *                                 is used.
 * @param lastSeenRoot             the no-rollback high-water-mark store.
 * @param accountKeyDeriver        derives the `username_hash` from the
 *                                 account_id (native by default).
 * @param inclusionGate            the inclusion-proof verifier (native by
 *                                 default; a test seam otherwise).
 */
public suspend fun verifyIdentityInLog(
    accountId: ByteArray,
    publishedSignaturePubkey: ByteArray,
    client: TransparencyClient?,
    operatorKey: ByteArray?,
    lastSeenRoot: LastSeenRootStore,
    accountKeyDeriver: AccountKeyDeriver = NativeAccountKeyDeriver,
    inclusionGate: InclusionGate = NativeInclusionGate,
    genesisGate: GenesisGate = NativeGenesisGate,
): LogVerifyResult {
    // OFF — the integration is unconfigured. Skip entirely. No latest
    // entry was read, so `isReset` is null.
    if (client == null || operatorKey == null) {
        return LogVerifyResult(LogVerifyStatus.OFF, isReset = null)
    }

    return try {
        // 1. Fetch the operator-signed root and run the no-rollback gate
        //    against our persisted high-water mark BEFORE trusting it.
        val root = client.fetchRoot()
        if (!lastSeenRoot.checkAndAdvance(root.logSize.toLong())) {
            // The log shrank — hard, unambiguous attack. (We do NOT
            // verify the root signature first: a shrink is suspicious
            // regardless of who signed it, and the rollback verdict is
            // the strongest.) We short-circuit BEFORE the latest lookup,
            // so no `isReset`.
            return LogVerifyResult(LogVerifyStatus.ROLLBACK, isReset = null)
        }

        // 2. Look up the account's latest logged entry.
        val usernameHash = accountKeyDeriver.derive(accountId)
        val latest = client.fetchLatest(usernameHash)
            ?: // Not yet logged. Benign under eventual consistency OR an
            //  off-log equivocation — surfaced, not auto-trusted. No
            //  latest entry, so no `isReset`.
            return LogVerifyResult(LogVerifyStatus.ABSENT, isReset = null)

        // From here a latest entry EXISTS — capture its `is_reset` marker
        // (KT-log S4). It rides out on both the MISMATCH and CONFIRMED
        // verdicts below; the caller surfaces it to distinguish an
        // owner-initiated reset from an unexplained key change.
        val isReset = latest.isReset

        // 3. The log's latest pubkey must equal what discovery served.
        //    A divergence is the equivocation signal.
        if (!latest.identityPubkey.contentEquals(publishedSignaturePubkey)) {
            return LogVerifyResult(LogVerifyStatus.MISMATCH, isReset = isReset)
        }

        // 4. Prove the exact (account_key, pubkey, version) leaf is
        //    included under the operator-signed root. A tampered proof,
        //    forged root, or substituted identity fails here.
        val proof = client.fetchProof(latest.idx)
        try {
            inclusionGate.verify(
                operatorKey = operatorKey,
                root = root,
                usernameHash = usernameHash,
                servedPubkey = publishedSignaturePubkey,
                proof = proof,
                version = latest.version,
            )
        } catch (_: FfiException) {
            // The inclusion / root-signature gate failed: the served key
            // is not provably committed by the pinned operator → treat as
            // a mismatch (a log/server divergence from what we were told).
            // A latest entry still existed, so the marker rides out.
            return LogVerifyResult(LogVerifyStatus.MISMATCH, isReset = isReset)
        }

        // 5. G-30 #2 (ADR-0019) — first-observation genesis check. If the
        //    latest entry carries a GENESIS self-signature (64 bytes — a
        //    rotation's 145-byte supersession proof is verified by the chain
        //    in verifyRotationChain, not here; a pre-Sprint-2 row carries
        //    none), confirm it authorises EXACTLY the served key. A failure
        //    means the entry's self-authorisation does not check out against
        //    the served key → MISMATCH. This never establishes trust at TOFU
        //    (the served key signs its own genesis); it is a consistency gate
        //    on the log entry's authorisation.
        val leafAuth = latest.leafAuth
        if (leafAuth != null && leafAuth.size == 64) {
            try {
                genesisGate.verify(leafAuth, usernameHash, publishedSignaturePubkey)
            } catch (_: FfiException) {
                return LogVerifyResult(LogVerifyStatus.MISMATCH, isReset = isReset)
            }
        }
        LogVerifyResult(LogVerifyStatus.CONFIRMED, isReset = isReset)
    } catch (_: TransparencyNetworkError) {
        LogVerifyResult(LogVerifyStatus.UNREACHABLE, isReset = null)
    } catch (_: TransparencyHttpError) {
        // A non-404 HTTP fault or a malformed body — the log is
        // effectively unusable for this check. Fail-open to UNREACHABLE
        // (the caller does not block on it); the supersession/pin gate is
        // the primary trust.
        LogVerifyResult(LogVerifyStatus.UNREACHABLE, isReset = null)
    }
}

/**
 * The outcome of [verifyRotationChain] (G-30 #2, ADR-0019).
 *
 * Extends the [verifyIdentityInLog] result with the chain-confirmed
 * per-account-key [version]: the transparency-log version the served
 * (rotated-to) key was proven included at, so the caller can persist it as
 * the next rotation's downgrade floor. [version] is non-`null` ONLY on a
 * [LogVerifyStatus.CONFIRMED] verdict (a latest entry was read AND the chain
 * gate passed); `null` on every other status (no chain confirmation to
 * anchor a version on).
 */
public data class RotationChainResult(
    /** The verdict — same union as [verifyIdentityInLog]. */
    val status: LogVerifyStatus,
    /** The `/latest` entry's `is_reset` marker when a latest entry was read
     *  ([LogVerifyStatus.CONFIRMED] / [LogVerifyStatus.MISMATCH]); `null`
     *  otherwise. */
    val isReset: Boolean?,
    /** The chain-confirmed log version on [LogVerifyStatus.CONFIRMED]; `null`
     *  otherwise. */
    val version: Long?,
)

/**
 * G-30 #2 (ADR-0019) — verify that a peer's KEY ROTATION is
 * cryptographically anchored to the operator-signed transparency log.
 *
 * Where [verifyIdentityInLog] proves a SINGLE served key is in the log, this
 * proves the TRANSITION old→new: the served (rotated-to) key is the log's
 * current head for the account AND a supersession chain — root signature,
 * no-rollback, Merkle inclusion, the embedded supersession proof from the
 * OLD pinned key, a strictly-newer logged version than the pinned floor, and
 * freshness — all hold. The heavy lifting is one native
 * `verifySupersessionChain` call (which THROWS on any chain-gate failure and
 * returns the authorised new pubkey on success).
 *
 * Called by [me.lakko.app.contacts.ContactsClient.checkTransparencyLog] ONLY
 * on the rotation branch (a pin exists, the served key differs from it, and
 * the bundle carried a supersession proof). It is BEST-EFFORT with the SAME
 * posture contract as [verifyIdentityInLog]: it never throws — every fault
 * maps to a status, and the caller (not this function) decides the posture.
 * Only [LogVerifyStatus.ROLLBACK] is surfaced as a hard verdict; a chain
 * contradiction is surfaced as [LogVerifyStatus.MISMATCH] (an equivocation
 * signal), NOT a block.
 *
 * Status mapping:
 *   * [LogVerifyStatus.OFF]         — the integration is unconfigured (no
 *                                     client / no pinned operator key).
 *                                     `version` null.
 *   * [LogVerifyStatus.ROLLBACK]    — the signed root's `log_size` shrank
 *                                     below our high-water mark. `version`
 *                                     null.
 *   * [LogVerifyStatus.ABSENT]      — `/latest` 404s (the rotated key is not
 *                                     yet logged), OR the log's latest pubkey
 *                                     is NOT the served key (a not-yet-logged
 *                                     rotation or a third key — we cannot
 *                                     chain-prove the served key, so we do
 *                                     NOT block; the standalone gate-1 result
 *                                     stands). `version` null.
 *   * [LogVerifyStatus.CONFIRMED]   — the chain gate passed. `version` is the
 *                                     latest entry's logged version.
 *   * [LogVerifyStatus.MISMATCH]    — the chain gate THREW (the log
 *                                     contradicts the proof — an equivocation
 *                                     signal). `version` null.
 *   * [LogVerifyStatus.UNREACHABLE] — a network/log fault. `version` null.
 *
 * @param accountId          the peer's raw 16-byte account_id.
 * @param servedPubkey       the 32-byte Ed25519 key discovery served (the
 *                           rotated-TO key).
 * @param supersessionProof  the bundle's `supersession_proof` (raw bytes) —
 *                           the chain's old-key authorisation.
 * @param pinnedOldPubkey    the locally-pinned OLD pubkey (32 bytes).
 * @param pinnedLogVersion   the pinned log-version floor (the downgrade
 *                           protection); `0` when never chain-confirmed.
 * @param client             pre-built client, or `null` to mean "off".
 * @param operatorKey        the pinned operator verifying key, or `null` for
 *                           "off".
 * @param lastSeenRoot       the no-rollback high-water-mark store.
 * @param accountKeyDeriver  derives the `username_hash` (native by default).
 * @param chainGate          the chain verifier (native by default; a test
 *                           seam otherwise).
 */
public suspend fun verifyRotationChain(
    accountId: ByteArray,
    servedPubkey: ByteArray,
    supersessionProof: ByteArray,
    pinnedOldPubkey: ByteArray,
    pinnedLogVersion: Long,
    client: TransparencyClient?,
    operatorKey: ByteArray?,
    lastSeenRoot: LastSeenRootStore,
    accountKeyDeriver: AccountKeyDeriver = NativeAccountKeyDeriver,
    chainGate: ChainGate = NativeChainGate,
): RotationChainResult {
    // OFF — the integration is unconfigured. Skip entirely.
    if (client == null || operatorKey == null) {
        return RotationChainResult(LogVerifyStatus.OFF, isReset = null, version = null)
    }

    return try {
        // 1. Fetch the operator-signed root and run the no-rollback gate
        //    against our high-water mark BEFORE trusting it.
        val root = client.fetchRoot()
        if (!lastSeenRoot.checkAndAdvance(root.logSize.toLong())) {
            return RotationChainResult(LogVerifyStatus.ROLLBACK, isReset = null, version = null)
        }

        // 2. Look up the account's latest logged entry.
        val usernameHash = accountKeyDeriver.derive(accountId)
        val latest = client.fetchLatest(usernameHash)
            ?: // The rotated key is not yet logged. Benign under eventual
            //  consistency — we cannot chain-prove it, so we do NOT block.
            return RotationChainResult(LogVerifyStatus.ABSENT, isReset = null, version = null)

        // 3. The log's latest pubkey must BE the served key for the chain to
        //    prove the served key. If it is a not-yet-logged rotation or a
        //    third key, we cannot chain-prove the served key here; the
        //    standalone gate-1 (supersession/pin) result stands. Surface
        //    ABSENT (no chain confirmation) — do NOT block on it.
        if (!latest.identityPubkey.contentEquals(servedPubkey)) {
            return RotationChainResult(
                LogVerifyStatus.ABSENT,
                isReset = latest.isReset,
                version = null,
            )
        }

        // 4. Fetch the inclusion proof and run the full chain gate. The
        //    verifier throws on any sub-gate failure (root sig / rollback /
        //    inclusion / supersession proof / non-newer version / freshness).
        val proof = client.fetchProof(latest.idx)
        try {
            chainGate.verify(
                operatorKey = operatorKey,
                root = root,
                usernameHash = usernameHash,
                servedPubkey = servedPubkey,
                version = latest.version,
                proof = proof,
                supersessionProof = supersessionProof,
                pinnedOldPubkey = pinnedOldPubkey,
                pinnedLogVersion = pinnedLogVersion.toULong(),
                lastSeenLogSize = lastSeenRoot.loadLastSeenLogSize().toULong(),
                nowUnixSecs = System.currentTimeMillis() / 1000L,
            )
        } catch (_: FfiException) {
            // The chain gate failed: the log CONTRADICTS the supersession
            // proof (an equivocation signal). Surface MISMATCH — the caller
            // records + warns but does NOT block (S3e posture). A latest
            // entry existed, so the marker rides out.
            return RotationChainResult(
                LogVerifyStatus.MISMATCH,
                isReset = latest.isReset,
                version = null,
            )
        }
        // The chain is proven — anchor the confirmed version for the next
        // rotation's downgrade floor.
        RotationChainResult(
            LogVerifyStatus.CONFIRMED,
            isReset = latest.isReset,
            version = latest.version.toLong(),
        )
    } catch (_: TransparencyNetworkError) {
        RotationChainResult(LogVerifyStatus.UNREACHABLE, isReset = null, version = null)
    } catch (_: TransparencyHttpError) {
        // A non-404 HTTP fault or a malformed body — the chain is unusable
        // for this check. Fail-open to UNREACHABLE (the caller does not block
        // on it); the standalone supersession/pin gate is primary.
        RotationChainResult(LogVerifyStatus.UNREACHABLE, isReset = null, version = null)
    }
}

/**
 * KT-log S4 — a LIGHTER, best-effort lookup of the `is_reset` marker for a
 * SERVED key, used to ENRICH a fail-closed `SupersessionRefused` (an
 * unproven changed key — the moment the reset/compromise distinction
 * matters most).
 *
 * Unlike [verifyIdentityInLog], this does NOT run the inclusion proof or
 * the no-rollback gate: it is a context-only enrichment that can never
 * change a trust decision (gate 1 throws regardless). It only answers:
 * "for this account, does the log's latest entry COMMIT TO EXACTLY the
 * served key, and was that entry an owner reset?"
 *
 * Returns:
 *   * `true`  — the log's latest pubkey matches [servedSignaturePubkey]
 *     AND its `is_reset` marker is set (owner-initiated fresh-start).
 *   * `false` — the log's latest pubkey matches [servedSignaturePubkey]
 *     AND its `is_reset` marker is NOT set (an unexplained change —
 *     possible compromise).
 *   * `null`  — the integration is off, the log is unreachable, the
 *     account is absent from the log, OR the log's latest pubkey does NOT
 *     match the served key (the marker is then about a different key and
 *     tells us nothing about the served one).
 *
 * Best-effort: ANY fault maps to `null`. This helper NEVER throws.
 *
 * @param accountId             the peer's raw 16-byte account_id.
 * @param servedSignaturePubkey the 32-byte Ed25519 key discovery served.
 * @param client                pre-built client, or `null` to mean
 *                              "integration off".
 * @param accountKeyDeriver     derives the `username_hash` from the
 *                              account_id (native by default).
 */
public suspend fun lookupServedKeyReset(
    accountId: ByteArray,
    servedSignaturePubkey: ByteArray,
    client: TransparencyClient?,
    accountKeyDeriver: AccountKeyDeriver = NativeAccountKeyDeriver,
): Boolean? {
    if (client == null) {
        return null
    }
    return try {
        val usernameHash = accountKeyDeriver.derive(accountId)
        val latest = client.fetchLatest(usernameHash)
            // Account not yet logged — no marker to read.
            ?: return null
        // The marker is only meaningful for the served key if the log's
        // latest entry actually commits to it. A divergence means the
        // marker describes some other key — return null (unknown).
        if (!latest.identityPubkey.contentEquals(servedSignaturePubkey)) {
            null
        } else {
            latest.isReset
        }
    } catch (_: Throwable) {
        // Best-effort enrichment — any fault is "unknown".
        null
    }
}
