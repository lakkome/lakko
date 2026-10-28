// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko

/// ADR-0081 decision 2 — the DeviceGroup commit outbox, driven over its
/// ports. Each failure path of the ADR's Context has a case:
///
///   1. a crash between the DS registration and the persist (before ADR-0081
///      the DS held N+1 and every device N): the entry written before the
///      send carries the commit, and the restart finishes it;
///   2. a failed publish after the persist (nothing retried it): the entry
///      stays and is retried;
///   3. a swallowed persist failure (the commit published while the device
///      stayed at N): making N+1 current fails closed and the entry stays.
///
/// The model is a tiny account: a durable epoch, a DS that registers an epoch
/// only together with its row, and an outbox slot. Mirrors
/// `apps/web/src/devices/device-group-commit.test.ts`.
@MainActor
final class DeviceGroupCommitTests: XCTestCase {

    private struct StepFailed: Error, Equatable { let step: String }

    private final class Proof: DsRequestSigner {
        let value: UInt64
        init(_ value: UInt64) { self.value = value }
        func epoch() -> UInt64 { value }
        func sign(method: String, path: String, query: String, body: Data) -> String { "proof@\(value)" }
    }

    @MainActor
    private final class Model {
        var durable: UInt64
        var slot: DeviceGroupCommitEntry?
        var journal: [(epoch: UInt64, row: String)] = []
        var registered: [UInt64: String] = [:]
        var log: [String] = []
        var failNext: [String: Int] = [:]

        init(startEpoch: UInt64 = 5) { durable = startEpoch }

        func step(_ name: String) throws {
            log.append(name)
            if let n = failNext[name], n > 0 {
                failNext[name] = n - 1
                throw StepFailed(step: name)
            }
        }

        func takeEpochElsewhere(_ epoch: UInt64) {
            registered[epoch] = "elsewhere"
            journal.append((epoch, "elsewhere"))
        }

        var ports: DeviceGroupCommitPorts {
            DeviceGroupCommitPorts(
                saveEntry: { [unowned self] entry in try self.step("save"); self.slot = entry },
                loadEntry: { [unowned self] in self.slot },
                deleteEntry: { [unowned self] in self.log.append("delete"); self.slot = nil },
                durableProof: { [unowned self] in Proof(self.durable) },
                commit: { [unowned self] entry, proof in
                    try self.step("commit")
                    let epoch = entry.epochValue ?? 0
                    if let held = self.registered[epoch] {
                        if held == entry.rowB64 {
                            let id = (self.journal.firstIndex { $0.epoch == epoch } ?? 0) + 1
                            return DsCommitAnswer(id: Int64(id), createdAt: "t", resent: true)
                        }
                        throw DsStatementError.conflict
                    }
                    guard proof.epoch() &+ 1 == epoch else { throw DsStatementError.malformed("proof epoch") }
                    self.registered[epoch] = entry.rowB64
                    self.journal.append((epoch, entry.rowB64))
                    return DsCommitAnswer(id: Int64(self.journal.count), createdAt: "t", resent: false)
                },
                makeCurrent: { [unowned self] entry in
                    try self.step("make-current")
                    self.durable = entry.epochValue ?? self.durable
                },
                announce: { [unowned self] _ in self.log.append("announce") },
                deliverWelcome: { [unowned self] _ in try self.step("welcome") }
            )
        }
    }

    private func entry(_ epoch: UInt64, welcome: Bool = true) -> DeviceGroupCommitEntry {
        DeviceGroupCommitEntry(
            kind: welcome ? .add : .remove,
            journalIdB64: Data("journal".utf8).base64EncodedString(),
            epoch: epoch,
            epochKeyB64: Data("key".utf8).base64EncodedString(),
            rowB64: Data("row-\(epoch)".utf8).base64EncodedString(),
            welcomeB64: welcome ? Data("welcome".utf8).base64EncodedString() : nil,
            welcomeRendezvousB64: welcome ? Data("rendezvous".utf8).base64EncodedString() : nil,
            nextSnapshotB64: Data("snap".utf8).base64EncodedString(),
            createdAtMs: 1)
    }

    // MARK: The order

    func testEntryBesideNThenTheCommitThenNPlusOneThenTheWelcomeThenDelete() async throws {
        let m = Model()
        let out = try await DeviceGroupCommitOutbox.run(entry(6), proof: Proof(5), ports: m.ports)
        XCTAssertEqual(out, .landed(messageId: 1))
        XCTAssertEqual(m.log, ["save", "commit", "make-current", "announce", "welcome", "delete"])
        XCTAssertEqual(m.durable, 6)
        XCTAssertNil(m.slot)
        XCTAssertEqual(m.journal.map(\.epoch), [6])
    }

    func testAFailedEntryWriteAbortsWithNothingSent() async {
        let m = Model()
        m.failNext["save"] = 1
        do {
            _ = try await DeviceGroupCommitOutbox.run(entry(6), proof: Proof(5), ports: m.ports)
            XCTFail("aborted")
        } catch {}
        XCTAssertTrue(m.journal.isEmpty)
        XCTAssertEqual(m.durable, 5)
    }

    func testAProofOfAnotherEpochIsRefusedBeforeAnythingIsWritten() async {
        let m = Model()
        do {
            _ = try await DeviceGroupCommitOutbox.run(entry(6), proof: Proof(4), ports: m.ports)
            XCTFail("refused")
        } catch is DeviceGroupCommitOutbox.ProofEpochMismatch {
        } catch {
            XCTFail("unexpected \(error)")
        }
        XCTAssertTrue(m.log.isEmpty)
    }

    // MARK: Context, path 1 — a crash after the DS took the commit, before the persist

    func testTheRestartReSendsTheSameRowAndMakesNPlusOneCurrent() async throws {
        let m = Model()
        m.failNext["make-current"] = 1
        do {
            _ = try await DeviceGroupCommitOutbox.run(entry(6), proof: Proof(5), ports: m.ports)
            XCTFail("killed")
        } catch {}
        XCTAssertEqual(m.durable, 5)
        XCTAssertEqual(m.journal.count, 1)
        XCTAssertNotNil(m.slot)

        let out = try await DeviceGroupCommitOutbox.resendOwed(ports: m.ports)
        XCTAssertEqual(out, .landed(messageId: 1))
        XCTAssertEqual(m.durable, 6)
        XCTAssertEqual(m.journal.count, 1, "nothing appended twice: the re-send answered the original row")
        XCTAssertNil(m.slot)
    }

    // MARK: Context, path 2 — the publish fails

    func testAFailedCommitKeepsTheEntryAndTheRetryLandsIt() async throws {
        let m = Model()
        m.failNext["commit"] = 1
        do {
            _ = try await DeviceGroupCommitOutbox.run(entry(6), proof: Proof(5), ports: m.ports)
            XCTFail("failed")
        } catch {}
        XCTAssertEqual(m.durable, 5)
        XCTAssertEqual(m.slot?.epoch, "6")
        XCTAssertTrue(m.journal.isEmpty)

        let out = try await DeviceGroupCommitOutbox.resendOwed(ports: m.ports)
        XCTAssertEqual(out, .landed(messageId: 1))
        XCTAssertEqual(m.durable, 6)
    }

    // MARK: Context, path 3 — the persist fails

    func testMakingNPlusOneCurrentFailsClosedAndTheWelcomeWaits() async throws {
        let m = Model()
        m.failNext["make-current"] = 1
        do {
            _ = try await DeviceGroupCommitOutbox.run(entry(6), proof: Proof(5), ports: m.ports)
            XCTFail("failed")
        } catch let error as StepFailed {
            XCTAssertEqual(error.step, "make-current")
        }
        XCTAssertFalse(m.log.contains("welcome"))
        XCTAssertNotNil(m.slot)
        _ = try await DeviceGroupCommitOutbox.resendOwed(ports: m.ports)
        XCTAssertEqual(m.durable, 6)
        XCTAssertEqual(m.log.filter { $0 == "welcome" }.count, 1)
    }

    func testAWelcomeThatFailsAfterNPlusOneIsCurrentIsDeliveredWithoutWritingNPlusOneAgain() async throws {
        let m = Model()
        m.failNext["welcome"] = 1
        do {
            _ = try await DeviceGroupCommitOutbox.run(entry(6), proof: Proof(5), ports: m.ports)
            XCTFail("failed")
        } catch {}
        XCTAssertEqual(m.durable, 6)
        m.log.removeAll()
        _ = try await DeviceGroupCommitOutbox.resendOwed(ports: m.ports)
        XCTAssertEqual(m.log, ["commit", "announce", "welcome", "delete"])
    }

    // MARK: Step 5 — a lost epoch

    func test409DeletesTheEntryAndTheDeviceStaysAtN() async throws {
        let m = Model()
        m.takeEpochElsewhere(6)
        let out = try await DeviceGroupCommitOutbox.run(entry(6), proof: Proof(5), ports: m.ports)
        XCTAssertEqual(out, .lost)
        XCTAssertNil(m.slot)
        XCTAssertEqual(m.durable, 5)
        XCTAssertFalse(m.log.contains("make-current"))
        XCTAssertFalse(m.log.contains("welcome"))
    }

    func testARetryThatMeets409DeletesTheEntryToo() async throws {
        let m = Model()
        m.failNext["commit"] = 1
        do {
            _ = try await DeviceGroupCommitOutbox.run(entry(6), proof: Proof(5), ports: m.ports)
        } catch {}
        m.takeEpochElsewhere(6)
        let out = try await DeviceGroupCommitOutbox.resendOwed(ports: m.ports)
        XCTAssertEqual(out, .lost)
        XCTAssertNil(m.slot)
        XCTAssertEqual(m.durable, 5)
    }

    func testNothingOwedIsANoOp() async throws {
        let m = Model()
        let out = try await DeviceGroupCommitOutbox.resendOwed(ports: m.ports)
        XCTAssertNil(out)
        XCTAssertTrue(m.log.isEmpty)
    }

    // MARK: Serialisation and the codec

    func testTwoCommitsOfOneAccountNeverRunAtOnce() async throws {
        let lock = DeviceGroupCommitLock()
        var order: [String] = []
        var release: CheckedContinuation<Void, Never>?
        let a = Task { @MainActor in
            try await lock.with(accountIdB64: "acc") {
                order.append("a-start")
                await withCheckedContinuation { release = $0 }
                order.append("a-end")
            }
        }
        // Let `a` start and park.
        while release == nil { await Task.yield() }
        let b = Task { @MainActor in
            try await lock.with(accountIdB64: "acc") { order.append("b") }
        }
        for _ in 0..<20 { await Task.yield() }
        XCTAssertEqual(order, ["a-start"])
        release?.resume()
        _ = try await a.value
        _ = try await b.value
        XCTAssertEqual(order, ["a-start", "a-end", "b"])
    }

    func testAnEntryRoundTripsAndAMalformedOneDecodesToNil() throws {
        let e = entry(9)
        XCTAssertEqual(DeviceGroupCommitEntry.decode(try e.encoded()), e)
        XCTAssertNil(DeviceGroupCommitEntry.decode(Data("{}".utf8)))
        XCTAssertNil(DeviceGroupCommitEntry.decode(Data("not json".utf8)))
        var half = entry(9, welcome: false)
        half.welcomeB64 = Data("x".utf8).base64EncodedString()
        XCTAssertNil(DeviceGroupCommitEntry.decode(try half.encoded()), "a Welcome without its rendezvous")
    }
}
