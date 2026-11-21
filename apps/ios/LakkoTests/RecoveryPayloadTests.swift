// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
import LakkoCore
@testable import Lakko

/// The another-device recovery row, v3 (G-56): the codec and the row framing
/// are the core's (`composeRecoveryPayload`, `decodeRecoveryPayload`,
/// `wrapRecoveryRow`, `unwrapRecoveryRow`); the golden vector and every
/// refusal are in ``EarlierKeysGateTests``. Here: the phrase rebuild and the
/// sender-to-receiver chain through the real core.
final class RecoveryPayloadTests: XCTestCase {

    // MARK: - Entropy -> phrase and seed (real core)

    func testTheTransferredEntropyRebuildsThePhraseAndItsSeed() throws {
        let mnemonic = RecoveryMnemonic.generate()
        let rebuilt = try RecoveryPayload.phraseAndSeed(fromEntropy: mnemonic.toEntropy())
        XCTAssertEqual(mnemonic.toEntropy().count, 16)
        XCTAssertEqual(rebuilt.phrase, mnemonic.phrase())
        XCTAssertEqual(rebuilt.seed, mnemonic.toSeed())
        XCTAssertEqual(rebuilt.seed.count, 64)
        XCTAssertEqual(PhraseWords.words(of: rebuilt.phrase).count, 12)
    }

    func testEntropyOfAnyOtherLengthIsRefused() {
        for count in [0, 15, 17, 32, 64] {
            XCTAssertThrowsError(
                try RecoveryPayload.phraseAndSeed(fromEntropy: Data(count: count)), "\(count) bytes")
        }
    }

    // MARK: - Seal / open round-trip (real core)

    /// The sender's chain (compose -> seal -> row) and the receiver's (row ->
    /// unwrap -> open -> decode -> phrase + seed + predecessors), under a
    /// 32-byte key of the shape the exporter returns.
    func testSenderToReceiverRoundTripThroughTheRealCore() throws {
        let key = Data(repeating: 0x33, count: Int(recoveryExporterKeyLen()))
        let mnemonic = RecoveryMnemonic.generate()
        let older = RecoveryMnemonic.generate().toSeed()
        let grant = Data(repeating: 0x05, count: 32)
        let payload = try composeRecoveryPayload(
            grant: grant,
            phraseEntropy: mnemonic.toEntropy(),
            predecessors: [FfiRecoveryPredecessor(seed: older, adoptedAtMs: 1_700_000_000_000)])
        XCTAssertEqual(payload.count, 49 + 72)

        let sealed = try sealWithKey(key: key, plaintext: payload)
        // ChaCha20-Poly1305 wire: nonce(12) || ciphertext+tag(16).
        XCTAssertEqual(sealed.count, 12 + payload.count + 16)

        let row = wrapRecoveryRow(sealed: sealed)
        XCTAssertEqual(row.prefix(25), Data("lakko/recovery-payload/v3".utf8))
        // The published row never carries the entropy or a seed in the clear.
        XCTAssertNil(row.range(of: mnemonic.toEntropy()))
        XCTAssertNil(row.range(of: older))
        let unwrapped = try XCTUnwrap(try unwrapRecoveryRow(row: row))

        let opened = try openWithKey(key: key, wire: unwrapped)
        let decoded = try decodeRecoveryPayload(plaintext: opened)
        XCTAssertEqual(decoded.grant, grant)
        XCTAssertEqual(decoded.predecessors.map(\.seed), [older])
        let rebuilt = try RecoveryPayload.phraseAndSeed(fromEntropy: decoded.phraseEntropy)
        XCTAssertEqual(rebuilt.phrase, mnemonic.phrase())
        XCTAssertEqual(rebuilt.seed, mnemonic.toSeed())
    }

    func testOpenRefusesWrongKey() throws {
        let key = Data(repeating: 0x33, count: 32)
        let wrongKey = Data(repeating: 0x34, count: 32)
        let payload = try composeRecoveryPayload(
            grant: Data(repeating: 0x41, count: 32),
            phraseEntropy: RecoveryMnemonic.generate().toEntropy(),
            predecessors: [])
        let sealed = try sealWithKey(key: key, plaintext: payload)
        XCTAssertThrowsError(try openWithKey(key: wrongKey, wire: sealed))
    }
}

/// Security follow-up of 2026-11-21: anyone who learned a rendezvous id can
/// post rows to it (one `rendezvous-write` token each), so every reader passes
/// over a row that does not open and reads on, and the gap look reads every
/// page instead of the first.
final class RendezvousReadersTests: XCTestCase {

    private func row(_ id: Int64, _ payload: Data) -> MailboxMessage {
        MailboxMessage(id: id, payload: payload, createdAt: "t")
    }

    private func junk(_ id: Int64) -> MailboxMessage {
        row(id, Data("junk-\(id)".utf8))
    }

    // MARK: - Reading a whole rendezvous

    func testAJunkFilledFirstPageDoesNotHideTheBundleOnALaterPage() async throws {
        let bundle = Data("the bundle".utf8)
        var asked: [(after: Int64, limit: Int)] = []
        let rows = try await RendezvousRead.readAll { after, limit in
            asked.append((after, limit))
            if after == 0 {
                return FetchResult(messages: (1...100).map { self.junk($0) }, hasMore: true)
            }
            return FetchResult(messages: [self.junk(101), self.row(102, bundle)], hasMore: false)
        }
        XCTAssertEqual(asked.map(\.after), [0, 100], "the next page starts after the last row id")
        XCTAssertEqual(Set(asked.map(\.limit)), [RendezvousRead.pageSize])
        XCTAssertEqual(rows.count, 102)
        let candidates = ChatRepository.gapLookCandidates(rows)
        XCTAssertEqual(candidates.first?.payload, bundle, "newest first")
        XCTAssertEqual(candidates.count, 102)
    }

    func testTheReadStopsAtThePageCap() async throws {
        var calls = 0
        let rows = try await RendezvousRead.readAll { after, limit in
            calls += 1
            let start = after + 1
            return FetchResult(messages: (start..<(start + Int64(limit))).map { self.junk($0) }, hasMore: true)
        }
        XCTAssertEqual(calls, RendezvousRead.pageCap)
        XCTAssertEqual(rows.count, RendezvousRead.pageCap * RendezvousRead.pageSize)
        XCTAssertEqual(RendezvousRead.pageSize, 100)
        XCTAssertEqual(RendezvousRead.pageCap, 10)
    }

    func testAnEmptyPageThatClaimsMoreEndsTheRead() async throws {
        var calls = 0
        let rows = try await RendezvousRead.readAll { _, _ in
            calls += 1
            return FetchResult(messages: [], hasMore: true)
        }
        XCTAssertEqual(calls, 1)
        XCTAssertTrue(rows.isEmpty)
    }

    func testTheGapLookSkipsRecoveryAndStateRows() {
        let state = StateBundle.wrapRow(Data([1, 2, 3]))
        let recovery = wrapRecoveryRow(sealed: Data([4, 5, 6]))
        let rows = [junk(1), row(2, state), row(3, recovery), junk(4)]
        XCTAssertEqual(ChatRepository.gapLookCandidates(rows).map(\.id), [4, 1])
    }

    // MARK: - The history bundle poll

    func testTheHistoryPollPassesOverARowThatDoesNotOpen() {
        let real = Data("real".utf8)
        let rows = [junk(2), row(3, StateBundle.wrapRow(Data([9]))), row(4, real)]
        XCTAssertEqual(WelcomeCourier.firstHistoryRow(rows, after: 1, opens: { $0 == real }), real)
        XCTAssertNil(WelcomeCourier.firstHistoryRow([junk(2)], after: 1, opens: { $0 == real }))
        // Without an opener the first untagged row is taken, as before.
        XCTAssertEqual(WelcomeCourier.firstHistoryRow(rows, after: 1, opens: nil), junk(2).payload)
    }

    // MARK: - The state bundle

    func testTheStateBundlePollPassesOverARowThatDoesNotOpen() throws {
        let key = Data(repeating: 0x21, count: 32)
        let bundle = StateBundle.V4(contacts: [], pins: [], knownGroupIdsB64: ["Zm9v"])
        let good = StateBundle.wrapRow(try sealWithKey(key: key, plaintext: StateBundle.encode(bundle)))
        let wrongKey = StateBundle.wrapRow(
            try sealWithKey(key: Data(repeating: 0x22, count: 32), plaintext: StateBundle.encode(bundle)))
        let undecodable = StateBundle.wrapRow(try sealWithKey(key: key, plaintext: Data("not json".utf8)))
        let rows = [junk(1), row(2, StateBundle.wrapRow(Data([1, 2]))), row(3, wrongKey), row(4, undecodable), row(5, good)]
        XCTAssertEqual(StateBundle.firstOpening(rows, after: 0, key: key), bundle)
        XCTAssertNil(StateBundle.firstOpening(Array(rows.prefix(4)), after: 0, key: key))
    }

    // MARK: - The recovery payload

    func testARecoveryRowThatDoesNotOpenIsPassedOver() throws {
        let key = Data(repeating: 0x33, count: Int(recoveryExporterKeyLen()))
        let payload = try composeRecoveryPayload(
            grant: Data(repeating: 5, count: 32),
            phraseEntropy: RecoveryMnemonic.generate().toEntropy(),
            predecessors: [])
        let sealed = try sealWithKey(key: key, plaintext: payload)
        let forged = try sealWithKey(key: Data(repeating: 0x44, count: key.count), plaintext: payload)
        let opens: (Data) -> Bool = { RecoveryPayload.opens(sealed: $0, key: key) }

        XCTAssertEqual(RecoveryPayload.pick(wrapRecoveryRow(sealed: forged), opens: opens), .passOver)
        XCTAssertEqual(RecoveryPayload.pick(Data("junk".utf8), opens: opens), .passOver)
        guard case .answer(.received(let got)) = RecoveryPayload.pick(wrapRecoveryRow(sealed: sealed), opens: opens) else {
            return XCTFail("the row that opens is the answer")
        }
        XCTAssertEqual(got, sealed)
    }
}
