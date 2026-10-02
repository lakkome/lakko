// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// Recovery-payload wire module: what an already signed-in device hands to
/// a new one over the pairing rendezvous mailbox ("another device" recovery).
///
/// Mirror of:
///
///   * `apps/web/src/devices/recovery-payload.ts`
///   * `apps/android/.../devices/RecoveryPayload.kt`
///
/// The wire format MUST stay byte-identical (cross-platform pairing:
/// an iOS scanner publishes rows a web joiner opens and vice versa).
///
/// After the add-device pairing completes, the existing device transfers
/// BOTH halves of recovery to the fresh device:
///
///   * **Access** — a single-use 32-byte device-authorization
///     `grant_token` minted by `POST /device/authorize/start`
///     (bearer). The fresh device redeems it anonymously at
///     `POST /device/authorize/redeem` for a session-token triple.
///   * **Keys** — the 16-byte entropy of the 12-word recovery phrase
///     (payload v2). The fresh device rebuilds the phrase from it, derives
///     the 64-byte AccountIdentity seed, checks that seed against the
///     published identity, and only then stores the seed AND the phrase. A
///     device added this way therefore holds the phrase like the device
///     that confirmed it, which is what the local three-word check, "Show
///     recovery phrase" and the encrypted backup need. v1 carried the
///     64-byte seed, from which the phrase cannot be rebuilt.
///
/// ## Crypto (MLS exporter)
///
/// Both devices sit at the same DeviceGroup epoch once the Welcome
/// is processed (the scanner committed the add; the joiner ratcheted
/// from the Welcome), so both derive the SAME 32-byte key via
///
/// ```text
///   key = deviceGroup.exportSecret(
///           identity,
///           "lakko/device-recovery/v2",   // recoveryExporterLabel
///           rendezvous_pseudonym,          // 32-byte, THIS pairing session
///           32,
///         )
/// ```
///
/// The scanner seals `grant_token(32) || phrase_entropy(16)` (48 bytes
/// fixed) with `sealWithKey(key, payload)` — ChaCha20-Poly1305, wire
/// `nonce(12) || ciphertext+tag` — and the joiner opens it with
/// `openWithKey`. The context binding to the rendezvous pseudonym
/// scopes the key to THIS pairing session.
///
/// ## Mailbox row format
///
/// The sealed blob rides the SAME rendezvous mailbox the Welcome
/// (row 1) and the optional history bundle (next row) use — no new
/// server surface. To keep the row types distinguishable the
/// recovery row carries a plaintext ASCII magic prefix:
///
/// ```text
///   row = "lakko/recovery-payload/v2" || sealed_blob
/// ```
///
/// The magic marks only the row TYPE; everything sensitive is
/// inside the AEAD blob. The server stores opaque bytes at a pseudonym
/// either way — the phrase entropy crosses the wire only sealed under
/// the exporter key, which the server cannot derive (it has no
/// DeviceGroup state).
///
/// ## No dual accept
///
/// v1 is refused by construction, in both directions: the row magic
/// differs (a v1 row is not recognised as a recovery row at all), the
/// exporter label differs (so the key differs), and the plaintext length
/// differs.
public enum RecoveryPayload {

    /// MLS exporter label for the per-pairing recovery key.
    /// Pinned ASCII; the `/v2` suffix is part of the wire contract.
    /// Keep in lockstep with the web / Android clients.
    public static let recoveryExporterLabel: String = "lakko/device-recovery/v2"

    /// Exporter output length — the ChaCha20-Poly1305 key size.
    public static let recoveryExporterKeyLength: Int = 32

    /// Raw device-authorization grant token length
    /// (`services/auth/src/device_authorize.rs::GRANT_TOKEN_LEN`).
    public static let recoveryGrantTokenLength: Int = 32

    /// Entropy length of the 12-word recovery phrase (BIP39, 128 bits).
    public static let recoveryPhraseEntropyLength: Int = 16

    /// Fixed plaintext payload length: `grant(32) || entropy(16)` = 48.
    public static let recoveryPayloadLength: Int =
        recoveryGrantTokenLength + recoveryPhraseEntropyLength

    /// Plaintext ASCII magic prefix that tags a rendezvous-mailbox
    /// row as a recovery payload (vs the Welcome / history-bundle
    /// rows). Byte-identical to the web's
    /// `new TextEncoder().encode("lakko/recovery-payload/v2")` —
    /// pure ASCII, so US-ASCII == UTF-8 here.
    private static let recoveryRowMagic: Data =
        Data("lakko/recovery-payload/v2".utf8)

    /// Server-side long-poll budget per request, in seconds — same
    /// rationale as ``WelcomeCourier``.
    private static let perRequestLongPollSeconds: Int = 15

    /// Default soft timeout for the joiner-side recovery poll. The
    /// scanner publishes the row right after the Welcome + history
    /// bundle (one extra `/device/authorize/start` round-trip),
    /// so a short window suffices; absence (the scanner holds no phrase,
    /// or its best-effort half failed) is tolerated — the join
    /// completes without a recovery payload.
    public static let recoveryPollTimeoutSeconds: TimeInterval = 15

    // MARK: - Payload build / parse

    /// Scanner side: concatenate `grant_token(32) || phrase_entropy(16)`
    /// into the fixed 48-byte plaintext payload. Strict length
    /// validation — a wrong-size grant or entropy is a caller bug that
    /// must surface before anything is sealed or published.
    ///
    /// The returned buffer is a fresh copy; the caller should zeroize
    /// it (and its inputs) once the sealed blob is built.
    public static func buildRecoveryPayload(
        grantToken: Data,
        phraseEntropy: Data
    ) throws -> Data {
        guard grantToken.count == recoveryGrantTokenLength else {
            throw RecoveryPayloadError.argument(
                "grant token must be \(recoveryGrantTokenLength) bytes " +
                "(got \(grantToken.count))"
            )
        }
        guard phraseEntropy.count == recoveryPhraseEntropyLength else {
            throw RecoveryPayloadError.argument(
                "phrase entropy must be \(recoveryPhraseEntropyLength) bytes " +
                "(got \(phraseEntropy.count))"
            )
        }
        var out = Data(capacity: recoveryPayloadLength)
        out.append(grantToken)
        out.append(phraseEntropy)
        return out
    }

    /// Parsed recovery payload — fresh copies of both halves.
    public struct ParsedRecoveryPayload {
        /// Raw 32-byte single-use device-authorization grant token.
        public let grantToken: Data
        /// Raw 16-byte entropy of the recovery phrase.
        public let phraseEntropy: Data
    }

    /// Joiner side: split the opened 48-byte plaintext back into
    /// `(grant_token, phrase_entropy)`. Strict length validation — any
    /// other length (a v1 payload is 96 bytes) is refused.
    ///
    /// Both returned buffers are fresh copies; the caller should
    /// zeroize the input `payload` after this returns.
    public static func parseRecoveryPayload(
        _ payload: Data
    ) throws -> ParsedRecoveryPayload {
        guard payload.count == recoveryPayloadLength else {
            throw RecoveryPayloadError.argument(
                "recovery payload must be \(recoveryPayloadLength) bytes " +
                "(got \(payload.count))"
            )
        }
        // Normalise to zero-based indices — `payload` may be a slice. The
        // copy holds the grant and the phrase entropy: zeroed in place once
        // the two fields are cut out of it (it is this function's only owner).
        var bytes = Data(payload)
        defer { bytes.resetBytes(in: 0..<bytes.count) }
        return ParsedRecoveryPayload(
            grantToken: bytes.subdata(in: 0..<recoveryGrantTokenLength),
            phraseEntropy: bytes.subdata(
                in: recoveryGrantTokenLength..<recoveryPayloadLength
            )
        )
    }

    /// The phrase and the 64-byte AccountIdentity seed a transferred
    /// entropy stands for. The phrase is the core's canonical form (12
    /// lower-case words, single spaces), which is what the device stores
    /// once the seed passed the published-identity check. Throws when the
    /// entropy is not 16 bytes.
    public static func phraseAndSeed(
        fromEntropy entropy: Data
    ) throws -> (phrase: String, seed: Data) {
        guard entropy.count == recoveryPhraseEntropyLength else {
            throw RecoveryPayloadError.argument(
                "phrase entropy must be \(recoveryPhraseEntropyLength) bytes " +
                "(got \(entropy.count))"
            )
        }
        let mnemonic = try RecoveryMnemonic.fromEntropy(entropy: entropy)
        return (mnemonic.phrase(), mnemonic.toSeed())
    }

    // MARK: - Mailbox row wrap / unwrap

    /// Wrap a sealed blob into the tagged mailbox row:
    /// `magic || sealed`.
    public static func wrapRecoveryRow(sealedBlob: Data) -> Data {
        var out = Data(capacity: recoveryRowMagic.count + sealedBlob.count)
        out.append(recoveryRowMagic)
        out.append(sealedBlob)
        return out
    }

    /// `true` when the mailbox row carries the recovery-payload magic
    /// prefix. Used by the joiner-side dispatch (and by the
    /// history-bundle poll to SKIP recovery rows).
    public static func isRecoveryPayloadRow(_ row: Data) -> Bool {
        guard row.count > recoveryRowMagic.count else { return false }
        return row.prefix(recoveryRowMagic.count) == recoveryRowMagic
    }

    /// Strip the magic prefix and return the sealed blob, or `nil`
    /// when the row is not a recovery-payload row.
    public static func unwrapRecoveryRow(_ row: Data) -> Data? {
        guard isRecoveryPayloadRow(row) else { return nil }
        // Normalise to zero-based indices — `row` may be a slice.
        let bytes = Data(row)
        return bytes.subdata(in: recoveryRowMagic.count..<bytes.count)
    }

    // MARK: - Publish / poll

    /// Scanner side: PUT the sealed recovery payload to the rendezvous
    /// mailbox AFTER the Welcome (and the optional history bundle).
    /// Same mailbox; the FIFO journal assigns it the next id. The row
    /// is wrapped with the plaintext magic so the joiner-side polls can
    /// dispatch by row type regardless of FIFO position.
    public static func publishRecoveryPayload(
        rendezvousPseudonym: Data,
        sealedBlob: Data,
        ds: DsClient = DsClient()
    ) async throws {
        _ = try await ds.publishRendezvous(id: rendezvousPseudonym,
            payload: wrapRecoveryRow(sealedBlob: sealedBlob)
        )
    }

    /// Discriminated outcome of
    /// ``pollForRecoveryPayload(rendezvousPseudonym:afterCursor:timeoutSeconds:ds:)``.
    public enum RecoveryPayloadOutcome {
        case received(Data)
        case timeout
        case cancelled
    }

    /// Joining side: poll the rendezvous mailbox past the Welcome
    /// cursor for the recovery-payload row. Non-recovery rows (the
    /// history bundle) are skipped by advancing the cursor; the first
    /// row carrying the magic prefix wins.
    ///
    /// Absence is an expected outcome (`.timeout`): a scanner that holds
    /// no recovery phrase, or whose best-effort recovery half failed,
    /// publishes no row — the join then completes without keys and the
    /// fresh device falls back to a passkey or phrase entry.
    ///
    /// `afterCursor` is typically the Welcome's DS journal id; both
    /// this poll and ``WelcomeCourier/pollForHistoryBundle`` start
    /// there and filter disjoint row types, so they can run
    /// sequentially or concurrently.
    public static func pollForRecoveryPayload(
        rendezvousPseudonym: Data,
        afterCursor: Int64,
        timeoutSeconds: TimeInterval = recoveryPollTimeoutSeconds,
        ds: DsClient = DsClient()
    ) async throws -> RecoveryPayloadOutcome {
        let deadline = Date(timeIntervalSinceNow: timeoutSeconds)
        var cursor = afterCursor

        while true {
            if Task.isCancelled { return .cancelled }
            let remaining = deadline.timeIntervalSinceNow
            if remaining <= 0 { return .timeout }

            let waitSecs = min(
                Self.perRequestLongPollSeconds,
                max(1, Int(remaining))
            )
            let result: FetchResult
            do {
                result = try await ds.fetchRendezvous(id: rendezvousPseudonym,
                    after: cursor,
                    limit: 1,
                    waitSecs: waitSecs
                )
            } catch DsClientError.http(let status, _, _) where status == 404 {
                // Same transient-error posture as the Welcome /
                // history polls — 404 means "no rows yet".
                continue
            } catch let err as DsClientError {
                if case .network = err {
                    continue
                }
                throw err
            }
            if let first = result.messages.first {
                if let sealed = unwrapRecoveryRow(first.payload) {
                    return .received(sealed)
                }
                // Not a recovery row (e.g. the history bundle) —
                // skip it.
                cursor = first.id
                continue
            }
            cursor = result.messages.reduce(cursor) { max($0, $1.id) }
        }
    }
}

/// Error vocabulary for ``RecoveryPayload``.
public enum RecoveryPayloadError: Error, LocalizedError {
    case argument(String)

    public var errorDescription: String? {
        switch self {
        case .argument(let message):
            return message
        }
    }
}
