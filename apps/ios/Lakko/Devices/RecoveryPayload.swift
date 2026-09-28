// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// Recovery-payload wire module — ADR-0018 Part 2a, factor (a)
/// ("another-device" recovery), Path B (MLS exporter).
///
/// Mirror of:
///
///   * `apps/web/src/devices/recovery-payload.ts`
///   * `apps/android/.../devices/RecoveryPayload.kt`
///
/// The wire format MUST stay byte-identical (cross-platform pairing:
/// an iOS scanner publishes rows a web joiner opens and vice versa).
///
/// After the add-device pairing completes, the scanner (existing,
/// authenticated) device transfers BOTH halves of recovery to the
/// fresh device over the rendezvous mailbox:
///
///   * **ACCESS** — a single-use 32-byte device-authorization
///     `grant_token` minted by `POST /device/authorize/start`
///     (bearer). The fresh device redeems it anonymously at
///     `POST /device/authorize/redeem` for a session-token triple.
///   * **IDENTITY** — the 64-byte AccountIdentity (BIP39 phrase)
///     seed (ADR-0017), so the fresh device never types the phrase.
///
/// ## Crypto (Path B — MLS exporter)
///
/// Both devices sit at the same DeviceGroup epoch once the Welcome
/// is processed (the scanner committed the add; the joiner ratcheted
/// from the Welcome), so both derive the SAME 32-byte key via
///
/// ```text
///   key = deviceGroup.exportSecret(
///           identity,
///           "lakko/device-recovery/v1",   // recoveryExporterLabel
///           rendezvous_pseudonym,          // 32-byte, THIS pairing session
///           32,
///         )
/// ```
///
/// The scanner seals `grant_token(32) || phrase_seed(64)` (96 bytes
/// fixed) with `sealWithKey(key, payload)` — ChaCha20-Poly1305, wire
/// `nonce(12) || ciphertext+tag` — and the joiner opens it with
/// `openWithKey`. The context binding to the rendezvous pseudonym
/// scopes the key to THIS pairing session.
///
/// ## Mailbox row format (DS opacity)
///
/// The sealed blob rides the SAME rendezvous mailbox the Welcome
/// (row 1) and the optional history bundle (next row) use — no new
/// server surface. To keep the three row types distinguishable the
/// recovery row carries a plaintext ASCII magic prefix:
///
/// ```text
///   row = "lakko/recovery-payload/v1" || sealed_blob
/// ```
///
/// The magic marks only the row TYPE; everything sensitive is
/// inside the AEAD blob. The DS stores opaque bytes at a pseudonym
/// either way (invariant #2) — the phrase seed crosses the wire
/// only sealed under the exporter key, which the server cannot
/// derive (it has no DeviceGroup state).
public enum RecoveryPayload {

    /// MLS exporter label for the per-pairing recovery key.
    /// Pinned ASCII; the `/v1` suffix is part of the wire contract.
    /// Keep in lockstep with the web / Android clients.
    public static let recoveryExporterLabel: String = "lakko/device-recovery/v1"

    /// Exporter output length — the ChaCha20-Poly1305 key size.
    public static let recoveryExporterKeyLength: Int = 32

    /// Raw device-authorization grant token length
    /// (`services/auth/src/device_authorize.rs::GRANT_TOKEN_LEN`).
    public static let recoveryGrantTokenLength: Int = 32

    /// AccountIdentity (BIP39 phrase) seed length (ADR-0017).
    public static let recoveryPhraseSeedLength: Int = 64

    /// Fixed plaintext payload length: `grant(32) || seed(64)`.
    public static let recoveryPayloadLength: Int =
        recoveryGrantTokenLength + recoveryPhraseSeedLength

    /// Plaintext ASCII magic prefix that tags a rendezvous-mailbox
    /// row as a recovery payload (vs the Welcome / history-bundle
    /// rows). Byte-identical to the web's
    /// `new TextEncoder().encode("lakko/recovery-payload/v1")` —
    /// pure ASCII, so US-ASCII == UTF-8 here.
    private static let recoveryRowMagic: Data =
        Data("lakko/recovery-payload/v1".utf8)

    /// Server-side long-poll budget per request, in seconds — same
    /// rationale as ``WelcomeCourier``.
    private static let perRequestLongPollSeconds: Int = 15

    /// Default soft timeout for the joiner-side recovery poll. The
    /// scanner publishes the row right after the Welcome + history
    /// bundle (one extra `/device/authorize/start` round-trip),
    /// so a short window suffices; absence (older scanner build, or
    /// the best-effort scanner half failed) is tolerated — the join
    /// completes without a recovery payload.
    public static let recoveryPollTimeoutSeconds: TimeInterval = 15

    // MARK: - Payload build / parse

    /// Scanner side: concatenate `grant_token(32) || phrase_seed(64)`
    /// into the fixed 96-byte plaintext payload. Strict length
    /// validation — a wrong-size grant or seed is a caller bug that
    /// must surface before anything is sealed or published.
    ///
    /// The returned buffer is a fresh copy; the caller should zeroize
    /// it (and its inputs) once the sealed blob is built.
    public static func buildRecoveryPayload(
        grantToken: Data,
        phraseSeed: Data
    ) throws -> Data {
        guard grantToken.count == recoveryGrantTokenLength else {
            throw RecoveryPayloadError.argument(
                "grant token must be \(recoveryGrantTokenLength) bytes " +
                "(got \(grantToken.count))"
            )
        }
        guard phraseSeed.count == recoveryPhraseSeedLength else {
            throw RecoveryPayloadError.argument(
                "phrase seed must be \(recoveryPhraseSeedLength) bytes " +
                "(got \(phraseSeed.count))"
            )
        }
        var out = Data(capacity: recoveryPayloadLength)
        out.append(grantToken)
        out.append(phraseSeed)
        return out
    }

    /// Parsed recovery payload — fresh copies of both halves.
    public struct ParsedRecoveryPayload {
        /// Raw 32-byte single-use device-authorization grant token.
        public let grantToken: Data
        /// Raw 64-byte AccountIdentity (BIP39 phrase) seed.
        public let phraseSeed: Data
    }

    /// Joiner side: split the opened 96-byte plaintext back into
    /// `(grant_token, phrase_seed)`. Strict length validation — any
    /// other length means a wire/protocol mismatch and is refused.
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
        // copy holds the grant and the phrase seed: zeroed in place once the
        // two fields are cut out of it (it is this function's only owner).
        var bytes = Data(payload)
        defer { bytes.resetBytes(in: 0..<bytes.count) }
        return ParsedRecoveryPayload(
            grantToken: bytes.subdata(in: 0..<recoveryGrantTokenLength),
            phraseSeed: bytes.subdata(
                in: recoveryGrantTokenLength..<recoveryPayloadLength
            )
        )
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
        _ = try await ds.publishMailbox(
            pseudonym: rendezvousPseudonym,
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
    /// Absence is an expected outcome (`.timeout`): an older scanner
    /// build, or a scanner whose best-effort recovery half failed,
    /// publishes no row — the join then completes exactly as today
    /// and the fresh device falls back to phrase entry.
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
                result = try await ds.fetchMailbox(
                    pseudonym: rendezvousPseudonym,
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
