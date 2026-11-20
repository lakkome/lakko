// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

// The supporters list (ADR-0097 section 2, `docs/ux/supporters.md` section
// 2): a static signed file next to the release notice, verified with the
// pinned release publisher key under its own domain. A build with no pinned
// key shows no list. Mirror of the web and Android handling.

/// A list that passed its signature check.
struct HeldSupportersList: Equatable {
    let listVersion: UInt64
    let names: [String]

    init(listVersion: UInt64, names: [String]) {
        self.listVersion = listVersion
        self.names = names
    }
}

/// The fetch, as the screen sees it.
enum SupportersFetchPhase: Equatable {
    case idle
    case loading
    case failed
}

/// What the Supporters screen shows (UX section 2.2).
enum SupportersScreenState: Equatable {
    /// Opened before the first fetch finished, with nothing held: a spinner.
    case loading
    /// The fetch failed with nothing held: the line and `Try again`.
    case failed
    /// The held list, names in file order.
    case names([String])
}

/// The pure rules, apart from any network or storage.
enum SupportersListPolicy {
    /// At most one fetch a day outside About (UX section 2.1).
    static let fetchIntervalMillis: Int64 = 24 * 60 * 60 * 1000

    /// A verified list replaces the held one only when it is newer; an older
    /// one is ignored, and the same version changes nothing.
    static func accepts(fetchedVersion: UInt64, held: HeldSupportersList?) -> Bool {
        guard let held else { return true }
        return fetchedVersion > held.listVersion
    }

    /// The About row shows only for a checked list with at least one name.
    static func showsRow(_ held: HeldSupportersList?) -> Bool {
        !(held?.names.isEmpty ?? true)
    }

    /// Whether a background fetch is due.
    static func isDue(lastFetchMillis: Int64?, nowMillis: Int64) -> Bool {
        guard let last = lastFetchMillis else { return true }
        return nowMillis - last >= fetchIntervalMillis || nowMillis < last
    }

    static func screenState(held: HeldSupportersList?, phase: SupportersFetchPhase) -> SupportersScreenState {
        if let held { return .names(held.names) }
        return phase == .failed ? .failed : .loading
    }

    /// The list's URL beside the release notice: same scheme and host,
    /// `/.well-known/lakko-supporters`.
    static func listUrl(manifestUrl: URL?) -> URL? {
        guard let manifestUrl,
              var components = URLComponents(url: manifestUrl, resolvingAgainstBaseURL: false),
              components.host?.isEmpty == false
        else { return nil }
        components.path = "/.well-known/lakko-supporters"
        components.query = nil
        components.fragment = nil
        return components.url
    }
}

/// Where the signed bytes of the held list are kept between runs. The
/// bytes are verified again when read, so a damaged file is just absent.
protocol SupportersListStorage: AnyObject {
    func load() -> Data?
    func save(_ bytes: Data)
}

/// A file in Application Support. Public data; nothing secret in it.
final class FileSupportersListStorage: SupportersListStorage {
    private let url: URL?

    init() {
        let dir = try? FileManager.default.url(
            for: .applicationSupportDirectory, in: .userDomainMask,
            appropriateFor: nil, create: true)
        url = dir?.appendingPathComponent("lakko-supporters.bin")
    }

    func load() -> Data? {
        guard let url else { return nil }
        return try? Data(contentsOf: url)
    }

    func save(_ bytes: Data) {
        guard let url else { return }
        do {
            try bytes.write(to: url, options: .atomic)
        } catch {
            NSLog("[supporters] list not saved: %@", String(describing: type(of: error)))
        }
    }
}

final class InMemorySupportersListStorage: SupportersListStorage {
    var bytes: Data?
    init(_ bytes: Data? = nil) { self.bytes = bytes }
    func load() -> Data? { bytes }
    func save(_ bytes: Data) { self.bytes = bytes }
}

/// Fetch the list. `nil` body on a non-2xx answer (a definitive "no list");
/// a thrown error when no answer came.
typealias SupportersListFetch = @Sendable (URL) async throws -> (status: Int, body: Data)

@MainActor
final class SupportersListCenter: ObservableObject {
    static let shared = SupportersListCenter()

    @Published private(set) var held: HeldSupportersList?
    @Published private(set) var phase: SupportersFetchPhase = .idle

    var showsRow: Bool { SupportersListPolicy.showsRow(held) }
    var screenState: SupportersScreenState {
        SupportersListPolicy.screenState(held: held, phase: phase)
    }

    private static let kLastFetch = "lakko.supporters.lastFetchAtMillis"

    private let key: () -> Data?
    private let url: () -> URL?
    private let fetch: SupportersListFetch
    private let verify: (Data, Data) throws -> VerifiedSupportersList
    private let storage: SupportersListStorage
    private let defaults: UserDefaults
    private var loaded = false
    private var inFlight: Task<Void, Never>?

    init(
        key: @escaping () -> Data? = { ReleasePin.verifyingKey },
        url: @escaping () -> URL? = { SupportersListPolicy.listUrl(manifestUrl: ReleasePin.manifestUrl) },
        fetch: @escaping SupportersListFetch = SupportersListNetwork.fetch,
        verify: @escaping (Data, Data) throws -> VerifiedSupportersList = {
            try verifySupportersList(bytes: $0, verifyingKey: $1)
        },
        storage: SupportersListStorage = FileSupportersListStorage(),
        defaults: UserDefaults = .standard
    ) {
        self.key = key
        self.url = url
        self.fetch = fetch
        self.verify = verify
        self.storage = storage
        self.defaults = defaults
    }

    /// Settings > About was shown: fetch now.
    func aboutOpened() async {
        await run(force: true)
    }

    /// The daily background check (app launch).
    func refreshIfDue(now: Date = Date()) async {
        await run(force: false, now: now)
    }

    /// `Try again` on the failed screen.
    func retry() async {
        await run(force: true)
    }

    private func run(force: Bool, now: Date = Date()) async {
        // No pinned key: no list at all (ADR-0097 section 2.3).
        guard let key = key(), let url = url() else {
            held = nil
            phase = .idle
            return
        }
        loadHeldIfNeeded(key: key)
        if let inFlight {
            await inFlight.value
            return
        }
        let nowMillis = Int64(now.timeIntervalSince1970 * 1000)
        let last = (defaults.object(forKey: Self.kLastFetch) as? NSNumber)?.int64Value
        guard force || SupportersListPolicy.isDue(lastFetchMillis: last, nowMillis: nowMillis) else { return }
        let task = Task { await self.fetchAndVerify(url: url, key: key, nowMillis: nowMillis) }
        inFlight = task
        await task.value
        inFlight = nil
    }

    private func loadHeldIfNeeded(key: Data) {
        guard !loaded else { return }
        loaded = true
        guard let bytes = storage.load() else { return }
        do {
            let list = try verify(bytes, key)
            held = HeldSupportersList(listVersion: list.listVersion, names: list.names)
        } catch {
            NSLog("[supporters] stored list failed its check, ignored")
        }
    }

    private func fetchAndVerify(url: URL, key: Data, nowMillis: Int64) async {
        phase = .loading
        let answer: (status: Int, body: Data)
        do {
            answer = try await fetch(url)
        } catch {
            NSLog("[supporters] list fetch failed: %@", String(describing: type(of: error)))
            phase = .failed
            return
        }
        defaults.set(NSNumber(value: nowMillis), forKey: Self.kLastFetch)
        guard (200..<300).contains(answer.status) else {
            NSLog("[supporters] list fetch HTTP %d", answer.status)
            phase = .failed
            return
        }
        do {
            let list = try verify(answer.body, key)
            if SupportersListPolicy.accepts(fetchedVersion: list.listVersion, held: held) {
                held = HeldSupportersList(listVersion: list.listVersion, names: list.names)
                storage.save(answer.body)
            }
            phase = .idle
        } catch {
            // A list that fails its check is ignored as if absent.
            NSLog("[supporters] fetched list failed its check, ignored")
            phase = .failed
        }
    }

}

enum SupportersListNetwork {
    /// The production fetch: through the anti-censorship transport when
    /// armed, direct otherwise; the signature is the trust anchor either way.
    static let fetch: SupportersListFetch = { url in
        var request = URLRequest(url: url)
        request.httpMethod = "GET"
        let (data, response) = try await AntiCensorshipTransport.perform(
            request, session: .shared, config: .current)
        let status = (response as? HTTPURLResponse)?.statusCode ?? 0
        return (status, data)
    }
}
