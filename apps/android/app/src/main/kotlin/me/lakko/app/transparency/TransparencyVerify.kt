// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.transparency

import uniffi.lakko_ffi_uniffi.FfiException
import uniffi.lakko_ffi_uniffi.transparencyAccountKey
import uniffi.lakko_ffi_uniffi.verifyLogInclusion

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
): LogVerifyStatus {
    // OFF — the integration is unconfigured. Skip entirely.
    if (client == null || operatorKey == null) {
        return LogVerifyStatus.OFF
    }

    return try {
        // 1. Fetch the operator-signed root and run the no-rollback gate
        //    against our persisted high-water mark BEFORE trusting it.
        val root = client.fetchRoot()
        if (!lastSeenRoot.checkAndAdvance(root.logSize.toLong())) {
            // The log shrank — hard, unambiguous attack. (We do NOT
            // verify the root signature first: a shrink is suspicious
            // regardless of who signed it, and the rollback verdict is
            // the strongest.)
            return LogVerifyStatus.ROLLBACK
        }

        // 2. Look up the account's latest logged entry.
        val usernameHash = accountKeyDeriver.derive(accountId)
        val latest = client.fetchLatest(usernameHash)
            ?: // Not yet logged. Benign under eventual consistency OR an
            //  off-log equivocation — surfaced, not auto-trusted.
            return LogVerifyStatus.ABSENT

        // 3. The log's latest pubkey must equal what discovery served.
        //    A divergence is the equivocation signal.
        if (!latest.identityPubkey.contentEquals(publishedSignaturePubkey)) {
            return LogVerifyStatus.MISMATCH
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
            return LogVerifyStatus.MISMATCH
        }
        LogVerifyStatus.CONFIRMED
    } catch (_: TransparencyNetworkError) {
        LogVerifyStatus.UNREACHABLE
    } catch (_: TransparencyHttpError) {
        // A non-404 HTTP fault or a malformed body — the log is
        // effectively unusable for this check. Fail-open to UNREACHABLE
        // (the caller does not block on it); the supersession/pin gate is
        // the primary trust.
        LogVerifyStatus.UNREACHABLE
    }
}
