// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.transparency

import uniffi.lakko_ffi_uniffi.FfiException
import uniffi.lakko_ffi_uniffi.transparencyAccountKey
import uniffi.lakko_ffi_uniffi.verifyGenesisAttestation
import uniffi.lakko_ffi_uniffi.verifyLeafGenesis
import uniffi.lakko_ffi_uniffi.verifyLogInclusion
import uniffi.lakko_ffi_uniffi.verifySupersessionChain
import uniffi.lakko_ffi_uniffi.verifyWitnessQuorum

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
 * Genesis-ATTEST-gate seam (ADR-0036, G-30 finding 2) — abstracts the native
 * `verifyGenesisAttestation` call so the first-observation genesis-attest
 * decision in [verifyIdentityInLog] is unit-testable on the host JVM without
 * the native lib + a real auth-signed attestation. The production
 * [NativeGenesisAttestGate] calls the real FFI, which THROWS [FfiException] on
 * a failed check (bad length / version / signature / any of the account /
 * username_hash / pubkey bindings mismatching); a test double can THROW (fail)
 * or RETURN (pass) — exactly that contract.
 */
public fun interface GenesisAttestGate {
    /**
     * Verify that [attestation] is a valid auth-attested genesis attestation
     * binding [expectedAccountId] / [expectedUsernameHash] / [expectedPubkey],
     * signed by [attestVerifyingKey] (the pinned auth attest key). No freshness
     * check (client first-observation path — the attestation is a permanent
     * historical artifact). MUST throw to signal a failure; returning normally
     * means all four bindings + the signature verified.
     */
    public fun verify(
        attestation: ByteArray,
        attestVerifyingKey: ByteArray,
        expectedAccountId: ByteArray,
        expectedUsernameHash: ByteArray,
        expectedPubkey: ByteArray,
    )
}

/** Production [GenesisAttestGate] — the real native `verifyGenesisAttestation`. */
public object NativeGenesisAttestGate : GenesisAttestGate {
    override fun verify(
        attestation: ByteArray,
        attestVerifyingKey: ByteArray,
        expectedAccountId: ByteArray,
        expectedUsernameHash: ByteArray,
        expectedPubkey: ByteArray,
    ) {
        // Throws FfiException on a failed check; the caller maps it to MISMATCH.
        verifyGenesisAttestation(
            attestation,
            attestVerifyingKey,
            expectedAccountId,
            expectedUsernameHash,
            expectedPubkey,
        )
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
 * Witness-QUORUM-gate seam (ADR-0039, G-30 finding-1) — abstracts the native
 * `verifyWitnessQuorum` call so the witness-gate decision in
 * [verifyIdentityInLog] / [verifyRotationChain] is unit-testable on the host
 * JVM without `liblakko_ffi_uniffi.so` + real Ed25519 co-sigs.
 *
 * The production [NativeWitnessGate] calls the real UniFFI
 * `verifyWitnessQuorum`, which THROWS [FfiException] when fewer than
 * `threshold` DISTINCT pinned witnesses each produced a valid co-signature
 * over the served root (or on a bad-length root / zero threshold) and RETURNS
 * when the K-of-N quorum is satisfied. A test double can THROW (sub-threshold
 * / invalid) or RETURN (satisfied) — exactly that contract. Generalises the
 * pre-ADR-0039 single-witness [verify] shape.
 */
public fun interface WitnessGate {
    /**
     * Verify that at least [threshold] DISTINCT keys in [witnessKeys] each
     * co-signed the served `(treeRoot, logSize)` among [cosigs]. MUST throw to
     * signal a sub-threshold / invalid quorum; returning normally means the
     * quorum verified.
     */
    public fun verify(
        cosigs: List<ByteArray>,
        witnessKeys: List<ByteArray>,
        threshold: Int,
        treeRoot: ByteArray,
        logSize: ULong,
    )
}

/** Production [WitnessGate] — the real native `verifyWitnessQuorum`. */
public object NativeWitnessGate : WitnessGate {
    override fun verify(
        cosigs: List<ByteArray>,
        witnessKeys: List<ByteArray>,
        threshold: Int,
        treeRoot: ByteArray,
        logSize: ULong,
    ) {
        // Throws FfiException when fewer than `threshold` distinct pinned
        // witnesses produced a valid co-sig over the served root (or on a
        // bad-length root / zero threshold). The caller maps a throw to a
        // fail-closed refusal of the root. The tally counts DISTINCT pinned
        // keys, so a foreign / replayed co-sig never inflates it.
        verifyWitnessQuorum(
            cosigs = cosigs,
            pinnedKeys = witnessKeys,
            threshold = threshold.toUInt(),
            expectedTreeRoot = treeRoot,
            expectedLogSize = logSize,
        )
    }
}

/** RFC-fixed byte length of one witness co-signature wire blob
 *  (`version ‖ tree_root(32) ‖ log_size(8) ‖ sig(64)`, ADR-0020). A
 *  malformed-length wire entry is dropped before the gate so one bad entry
 *  cannot sink an otherwise-valid quorum (the quorum verifier re-checks each
 *  entry's length anyway, but pre-filtering is fail-safe). */
private const val WITNESS_COSIG_LEN: Int = 105

/**
 * ADR-0039 (G-30 finding-1) — the K-of-N witness-QUORUM gate over a fetched
 * root, generalising the single-witness co-sig gate of ADR-0020. Returns
 * `true` when the gate REFUSES the root (the caller then fails closed →
 * MISMATCH), `false` when it passes (or the gate is off). Mirror of the web
 * `witnessGateRefuses`:
 *   * empty [witnessKeys] set → gate OFF, never refuses (the operator-only
 *     transition posture — subsumes the old `witnessKey == null` case).
 *   * pinned key set + fewer than [threshold] DISTINCT pinned witnesses with a
 *     valid co-sig over the served `(treeRoot, logSize)` → REFUSE. This
 *     subsumes the old "no co-sig at all" case (0 valid < threshold) and any
 *     bad-length / zero-threshold input (the quorum verifier throws → refuse).
 *   * pinned key set + a quorum of ≥ [threshold] distinct valid co-sigs →
 *     pass. Foreign / garbage / replayed co-sigs never inflate the tally.
 */
private fun witnessGateRefuses(
    root: SignedRoot,
    witnessKeys: List<ByteArray>,
    threshold: Int,
    witnessGate: WitnessGate,
): Boolean {
    // Gate off — no witnesses pinned (operator-only transition posture).
    if (witnessKeys.isEmpty()) return false
    return try {
        // Drop any wire cosig that is not exactly one blob — a malformed-length
        // entry can never be a valid witness co-sig, and dropping it keeps one
        // bad entry from sinking an otherwise-valid quorum (fail-safe). The
        // keys are already 32 bytes each (config drops off-length pins).
        val cosigs = root.witnessCosigs.filter { it.size == WITNESS_COSIG_LEN }
        witnessGate.verify(cosigs, witnessKeys, threshold, root.treeRoot, root.logSize)
        false
    } catch (_: Throwable) {
        // Fewer than `threshold` distinct pinned witnesses produced a valid
        // co-signature over the served root (or a bad-length / zero-threshold
        // input) → fail closed. Catch Throwable (not just FfiException) so ANY
        // unexpected error from the quorum verifier fails closed, matching the
        // web / iOS catch-all robustness (ADR-0039 F3).
        true
    }
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
 * @param witnessKeys              ADR-0039 (G-30 finding-1) — the pinned
 *                                 WITNESS verifying key SET (each 32 bytes),
 *                                 or `[]` for "no witness gate" (the
 *                                 operator-only transition posture). When
 *                                 non-empty, a fetched root MUST carry a
 *                                 K-of-N QUORUM of valid witness co-signatures
 *                                 over its `(tree_root, log_size)` — at least
 *                                 [witnessThreshold] DISTINCT pinned witnesses
 *                                 — or the verify fails closed (MISMATCH).
 * @param witnessThreshold         ADR-0039 — the K-of-N quorum threshold (the
 *                                 minimum number of DISTINCT pinned witnesses
 *                                 that must have co-signed). Only consulted
 *                                 when [witnessKeys] is non-empty; a threshold
 *                                 above the pinned-key count makes the gate
 *                                 refuse every root (fail-closed).
 * @param accountKeyDeriver        derives the `username_hash` from the
 *                                 account_id (native by default).
 * @param inclusionGate            the inclusion-proof verifier (native by
 *                                 default; a test seam otherwise).
 * @param genesisAttestKey         ADR-0036 — the pinned auth attest
 *                                 verifying key (32 bytes; reuses the
 *                                 device-attest key per ADR-0036 §3), or
 *                                 `null` to skip the genesis-attest check
 *                                 (the key is unresolved / unpinned). When
 *                                 present AND the latest entry carries a
 *                                 served attestation, an invalid attestation
 *                                 is fail-closed to MISMATCH.
 */
public suspend fun verifyIdentityInLog(
    accountId: ByteArray,
    publishedSignaturePubkey: ByteArray,
    client: TransparencyClient?,
    operatorKey: ByteArray?,
    lastSeenRoot: LastSeenRootStore,
    witnessKeys: List<ByteArray>,
    witnessThreshold: Int = 1,
    genesisAttestKey: ByteArray? = null,
    accountKeyDeriver: AccountKeyDeriver = NativeAccountKeyDeriver,
    inclusionGate: InclusionGate = NativeInclusionGate,
    genesisGate: GenesisGate = NativeGenesisGate,
    genesisAttestGate: GenesisAttestGate = NativeGenesisAttestGate,
    witnessGate: WitnessGate = NativeWitnessGate,
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

        // 1b. ADR-0039 — the K-of-N witness-quorum gate. When a witness key
        //     SET is pinned, the root must carry a QUORUM of valid co-sigs
        //     over its (tree_root, log_size) — at least `witnessThreshold`
        //     DISTINCT pinned witnesses — or we fail closed: an operator that
        //     forks cannot get both forks co-signed by K independent witnesses
        //     (the split-view defence, now requiring K corrupt witnesses, not
        //     one). Surfaced as MISMATCH (the root is untrusted, so any
        //     inclusion proof against it is meaningless). No latest read yet.
        if (witnessGateRefuses(root, witnessKeys, witnessThreshold, witnessGate)) {
            return LogVerifyResult(LogVerifyStatus.MISMATCH, isReset = null)
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

        // 5b. ADR-0036 (G-30 finding 2) — first-observation genesis-ATTEST
        //     check, ADDITIVE to the self-sig above. If the latest entry
        //     carries an auth-attested genesis (the S4-served genesis_attest)
        //     AND we hold the pinned auth attest verifying key, verify all
        //     four bindings (account_id, username_hash, served pubkey,
        //     signature). The self-sig only DETECTS squatting (its own key
        //     can self-sign a fabricated binding); this PREVENTS it — only the
        //     OPAQUE-authenticated account could have minted the attestation.
        //     Fail-closed: a served attestation that does not verify → MISMATCH
        //     (never blind-trust). Skipped when the key is unpinned/unresolved
        //     or no attestation is served (additive, flag-gated server-side).
        val genesisAttest = latest.genesisAttest
        if (genesisAttest != null && genesisAttestKey != null) {
            try {
                genesisAttestGate.verify(
                    attestation = genesisAttest,
                    attestVerifyingKey = genesisAttestKey,
                    expectedAccountId = accountId,
                    expectedUsernameHash = usernameHash,
                    expectedPubkey = publishedSignaturePubkey,
                )
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
 * @param witnessKeys        ADR-0039 (G-30 finding-1) — the pinned WITNESS
 *                           verifying key SET (each 32 bytes), or `[]` for "no
 *                           witness gate". When non-empty, a fetched root
 *                           lacking a K-of-N quorum of valid co-sigs is
 *                           refused (fail-closed → MISMATCH).
 * @param witnessThreshold   ADR-0039 — the K-of-N quorum threshold. Only
 *                           consulted when [witnessKeys] is non-empty.
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
    witnessKeys: List<ByteArray>,
    witnessThreshold: Int = 1,
    accountKeyDeriver: AccountKeyDeriver = NativeAccountKeyDeriver,
    chainGate: ChainGate = NativeChainGate,
    witnessGate: WitnessGate = NativeWitnessGate,
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

        // 1b. ADR-0039 — the K-of-N witness-quorum gate (see
        //     `witnessGateRefuses`). When a witness key SET is pinned, a root
        //     lacking a quorum of valid co-sigs is refused (the split-view
        //     defence, fail-closed); surfaced as MISMATCH. No latest read yet.
        if (witnessGateRefuses(root, witnessKeys, witnessThreshold, witnessGate)) {
            return RotationChainResult(LogVerifyStatus.MISMATCH, isReset = null, version = null)
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
