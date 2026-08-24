// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCoreExtension

/// Production wiring for the iOS silent-push NSE background decrypt
/// (ADR-0041 Phase 3) — the iOS counterpart of Android's
/// `PushBackgroundDecrypt.kt` and the web `sw.js` `buildDecryptDeps`.
///
/// Everything here touches the shared on-device stores (Keychain / App Group
/// container) or the SLIM `LakkoCoreExtension` UniFFI framework (`Identity` /
/// `Group` / `parseBodyEnvelope`) — the decrypt-only surface the NSE links
/// INSTEAD of the full 333 MB `LakkoCore`, to stay under its ~30 MB RAM
/// budget. So it is NOT unit-tested; the pure, FFI-free decrypt core in
/// `NsePushDecrypt.swift` carries the tested logic and this file supplies the
/// real ``NsePushDecryptDeps``.
///
/// ## READ-ONLY THROWAWAY — the load-bearing invariant
///
/// The ``NsePushDecryptDeps`` this file builds has NO write path, and
/// ``NseRealThrowawayGroup`` frees the throwaway handles WITHOUT ever calling
/// `snapshotStorage()` / `IdentitySnapshotStore.putSnapshot` — the slim FFI
/// does not even EXPOSE a persist method (see `crates/lakko-ffi-extension`),
/// so there is no write path to reach. Persisting the advanced ratchet would
/// desync the foreground app, which restores from the SAME last-persisted
/// snapshot and does the real durable processing from its OWN cursor (the
/// G-34 nonce-reuse-on-crash class). There is intentionally no persist call
/// anywhere in this file.

/// Builds the real, read-only ``NsePushDecryptDeps`` from the shared stores +
/// the slim FFI. `resolveAccount` reverses the wake's group pseudonym to an
/// account straight from ``KnownGroupsStore`` (the NSE has no live
/// `ChatRepository`); `openThrowawayGroup` reconstructs a FRESH throwaway
/// `Identity` + `Group` — never the app's handles (the NSE is a separate
/// process anyway); `dsFetch` is a slim `URLSession` GET (``NseDsFetch``, no
/// anti-censorship transport pulled in); `startCursor` reads the per-group
/// cursor from ``MessageLogStore`` (its FFI-clean half).
struct NseRealDecryptDeps: NsePushDecryptDeps {

    private let knownGroups = KnownGroupsStore()
    private let snapshotStore = IdentitySnapshotStore()
    private let messageLog = MessageLogStore()
    private let pushRoutes = PushRoutingStore()
    private let dsBaseURL: String
    private let session: URLSession

    init(dsBaseURL: String = NseConfig.dsBaseURL, session: URLSession = .shared) {
        self.dsBaseURL = dsBaseURL
        self.session = session
    }

    /// ADR-0042 Sprint 4 — the App-Group reverse-index read that turns the
    /// wake's opaque `push_gid` hex back into the real `(account, group_id)`.
    /// Reuses ``PushRoutingStore`` (compiled into the NSE target as a sibling
    /// source, like the other shared stores): it is FFI-clean — Foundation +
    /// CryptoKit only, no `LakkoCore` — and `lookupRoute` is a plain
    /// shared-suite `UserDefaults` read (the Keychain-touching secret paths
    /// exist on the store but are never called from the NSE), so the one
    /// `me.lakko.push-route.v1.` key-shape definition is shared with the app
    /// instead of duplicated here. READ-ONLY.
    func lookupRoute(pushGidHex: String) -> (accountIdB64: String, groupIdHex: String)? {
        pushRoutes.lookupRoute(pushGidHex: pushGidHex)
    }

    func resolveAccount(groupHex: String) -> NseResolvedAccount? {
        guard let accountIdB64 = knownGroups.findAccountForGroup(groupIdHex: groupHex),
              let groupIdBytes = Self.hexToData(groupHex) else {
            return nil
        }
        return NseResolvedAccount(accountIdB64: accountIdB64, groupIdBytes: groupIdBytes)
    }

    func loadIdentitySeed(accountIdB64: String) -> Data? {
        // A genuine Keychain fault is swallowed to `nil` — the NSE degrades
        // to the generic banner, never crashes.
        (try? AccountIdentitySeedStore.getAccountIdentitySeed(accountIdB64: accountIdB64)) ?? nil
    }

    func loadSnapshot(accountIdB64: String) -> Data? {
        snapshotStore.getSnapshot(accountIdB64: accountIdB64)
    }

    func openThrowawayGroup(
        seed: Data,
        snapshot: Data,
        groupIdBytes: Data
    ) throws -> NseThrowawayGroup? {
        // A FRESH throwaway Identity — never the app's live handle (the NSE
        // is a separate process, so there is none anyway). Restoring lays
        // down provider storage in THIS temp Identity only.
        let identity = try Identity.fromSeed(seed: seed)
        try identity.restoreStorage(snapshot: snapshot)
        // `nil` → not a member of this group (or the group is not in the
        // restored provider) — the core discards + shows the generic banner.
        guard let group = try loadGroup(identity: identity, groupId: groupIdBytes) else {
            return nil
        }
        return NseRealThrowawayGroup(identity: identity, group: group)
    }

    func dsFetch(groupIdBytes: Data, afterId: Int64) async throws -> [NseFetchedRow] {
        try await NseDsFetch.fetchGroup(
            baseURL: dsBaseURL,
            session: session,
            groupId: groupIdBytes,
            after: afterId
        )
    }

    func startCursor(accountIdB64: String, groupHex: String) -> Int64? {
        messageLog.getLog(accountIdB64: accountIdB64)?.groups[groupHex]?.cursor
    }

    /// ADR-0041 Finding-1 — read the armed flag the app mirrors into the
    /// shared App Group suite. Read-only: the NSE never writes it. When
    /// armed, ``decryptPushPreview`` short-circuits before any DS fetch, so
    /// the NSE makes no direct connection. Absent / an unprovisioned suite →
    /// `false` (the common disarmed case keeps the rich preview; a truly
    /// unprovisioned App Group makes the decrypt fail closed on the missing
    /// shared stores anyway).
    func isAntiCensorshipArmed() -> Bool {
        SharedStorage.isAntiCensorshipArmed()
    }

    /// Decode a lowercase-hex string into bytes. `nil` on any malformed input
    /// (odd length, non-hex char).
    static func hexToData(_ hex: String) -> Data? {
        guard hex.count % 2 == 0 else { return nil }
        var out = Data(capacity: hex.count / 2)
        var index = hex.startIndex
        while index < hex.endIndex {
            let next = hex.index(index, offsetBy: 2)
            guard let byte = UInt8(hex[index..<next], radix: 16) else { return nil }
            out.append(byte)
            index = next
        }
        return out
    }
}

/// A THROWAWAY MLS group backed by a temporary `Identity` + `Group` restored
/// from the last-persisted snapshot.
///
/// ## READ-ONLY THROWAWAY — load-bearing
///
/// ``process(_:)`` runs the audited `processIncomingVerifyingRoles` receive
/// path (advancing THIS temp provider's ratchet in memory) but NEVER persists
/// it, and ``close()`` drops the handles WITHOUT any `snapshotStorage()` /
/// `putSnapshot` call — the slim FFI exposes no such method. The Rust handles
/// are freed by UniFFI's Swift `deinit` when the references are released, so
/// the background decrypt leaves zero durable MLS state behind.
private final class NseRealThrowawayGroup: NseThrowawayGroup {

    private let identity: Identity
    private let group: Group

    init(identity: Identity, group: Group) {
        self.identity = identity
        self.group = group
    }

    func process(_ payload: Data) -> NseProcessedRow? {
        // ADR-0022 — the same role-verifying receive path the live poll loop
        // uses. A bad frame / wrong epoch / own echo / role-refused commit
        // throws and is dropped (fail-closed, invariant #4).
        let processed: ProcessedMessage
        do {
            processed = try group.processIncomingVerifyingRoles(identity: identity, bytes: payload)
        } catch {
            return nil
        }
        // Only Application messages carry a user-visible preview. A Commit /
        // Proposal / ExternalJoinProposal advances state (in the THROWAWAY
        // provider only) but has no body to show.
        guard case let .application(data, _) = processed else { return nil }
        // SERVICE-ENVELOPE FILTER — the background half of the gate the
        // foreground `ChatClient.toEvent` has always run, in the same order.
        // Call-signaling proto bytes, ADR-0042/0044 push-routing-secret
        // envelopes and ADR-0045 committer-liveness beacons all ride the SAME
        // group journal as chat; without this, `parseBodyEnvelope`'s deliberate
        // legacy UTF-8 fallback degrades them to a `.chat` body and the NSE
        // renders a notification full of a domain string plus raw binary.
        // Returning nil drops the row, so the caller's loop falls back to the
        // newest REAL message (or the generic banner).
        if nseIsServiceEnvelope(
            data,
            decodesAsPushRoutingSecret: Self.decodesAsPushRoutingSecret,
            decodesAsCommitterBeacon: Self.decodesAsCommitterBeacon,
            committerBeaconEnvelopeLen: Self.committerBeaconEnvelopeLength
        ) {
            return nil
        }
        let body = parseBodyEnvelope(bytes: data)
        let kind: NseProcessedKind
        switch body.kind {
        case .chat: kind = .chat
        case .edit: kind = .edit
        case .delete: kind = .delete
        case .reaction: kind = .reaction
        case .typing: kind = .typing
        case .readReceipt: kind = .readReceipt
        }
        return NseProcessedRow(
            kind: kind,
            text: body.text,
            attachmentCount: body.attachments.count
        )
    }

    /// The AUTHORITATIVE half of the push-routing-secret filter: the slim FFI's
    /// `decodePushRoutingSecret` (ADR-0042 v2, per ADR-0044), whose domain +
    /// version + exact-length gate is the ONE definition of that wire format.
    /// A throw means "not this envelope" — the common case for every chat
    /// message — so the caller falls through to the normal chat parse.
    ///
    /// Deliberately routed through the Rust codec rather than a Swift-side
    /// domain-prefix match: a hardcoded prefix silently drifts on the next
    /// envelope version bump.
    ///
    /// READ-ONLY: a pure decode. It decodes the secret bytes but NOTHING here
    /// persists, derives a `push_gid`, or subscribes — that is the foreground
    /// app's job on its own durable path. The NSE only needs the yes/no.
    private static func decodesAsPushRoutingSecret(_ bytes: Data) -> Bool {
        (try? decodePushRoutingSecret(plaintext: bytes)) != nil
    }

    /// The AUTHORITATIVE — and here COMPLETE — half of the committer-beacon
    /// filter: the slim FFI's `decodeCommitterBeacon` (ADR-0045). It returns
    /// nothing on success, because the beacon is FIELD-LESS: a clean return IS
    /// the whole verdict, "this is a service envelope, render nothing".
    ///
    /// Unlike the routing case, the Swift-side peek in front of this is only a
    /// hot-path guard and never a partial verdict — every beacon on the wire is
    /// the same ``committerBeaconEnvelopeLength`` bytes, so this one decode is a
    /// total test of that value.
    ///
    /// READ-ONLY: a pure decode. The NSE never EMITS a beacon and never folds a
    /// committer verdict — a beacon means "able to sweep", and this path
    /// structurally cannot commit (C-59), so a beacon from it would be a lie
    /// that costs the group a takeover. Only the yes/no is used.
    private static func decodesAsCommitterBeacon(_ bytes: Data) -> Bool {
        (try? decodeCommitterBeacon(plaintext: bytes)) != nil
    }

    /// The exact composed length of an ADR-0045 beacon, read from the slim FFI's
    /// `committerBeaconEnvelopeLen()` — which derives it from `lakko_contacts`'
    /// own domain + payload constants — so the NSE's cheap pre-filter carries no
    /// hand-copied wire number. This is the FFI-side half of the injection
    /// ``nseLooksLikeCommitterBeacon(_:envelopeLen:)`` describes; a Rust golden
    /// pins the exported value against the composed wire, so a revision fails
    /// Rust CI instead of silently narrowing this filter.
    ///
    /// READ-ONLY: a pure constant. It opens no write path and cannot weaken the
    /// read-only-throwaway discipline (C-59).
    private static var committerBeaconEnvelopeLength: Int {
        Int(committerBeaconEnvelopeLen())
    }

    /// LOAD-BEARING: this does NOT persist — no `snapshotStorage()` /
    /// `putSnapshot`. Dropping the references lets UniFFI's `deinit` free the
    /// Rust handles; nothing durable is written.
    func close() {
        // Intentionally empty — ARC + UniFFI `deinit` reclaim the handles.
        // The method exists for the ``NseThrowawayGroup`` contract + to make
        // the "discarded, never persisted" step explicit at the call site.
    }
}

/// Slim, self-contained DS group-journal fetch for the NSE.
///
/// A plain `URLSession` GET of `GET /ds/group/{group_hex}/messages?after=&wait=0`
/// — DELIBERATELY NOT the app's ``DsClient``, which routes through
/// `AntiCensorshipTransport` and would drag the transport / anti-censorship
/// stack (tokio, REALITY, MASQUE, …) into the memory-starved NSE process. The
/// NSE only needs a read-only catch-up fetch; this is the whole surface.
enum NseDsFetch {

    /// Fetch DS journal rows strictly after `after` for `groupId`. `wait=0` —
    /// a catch-up fetch, never a long-poll (the NSE budget is short).
    static func fetchGroup(
        baseURL: String,
        session: URLSession,
        groupId: Data,
        after: Int64
    ) async throws -> [NseFetchedRow] {
        let hex = hexString(groupId)
        guard var components = URLComponents(string: baseURL + "/ds/group/\(hex)/messages") else {
            throw NseDsFetchError.badURL
        }
        components.queryItems = [
            URLQueryItem(name: "after", value: String(after)),
            URLQueryItem(name: "wait", value: "0"),
        ]
        guard let url = components.url else { throw NseDsFetchError.badURL }

        var request = URLRequest(url: url)
        request.httpMethod = "GET"
        // Keep well inside the NSE wall-clock budget (the handler also runs a
        // shorter overall deadline).
        request.timeoutInterval = 15

        let (data, response) = try await session.data(for: request)
        guard let http = response as? HTTPURLResponse,
              (200..<300).contains(http.statusCode) else {
            throw NseDsFetchError.http
        }
        let decoded = try JSONDecoder().decode(FetchWireResponse.self, from: data)
        return decoded.messages.compactMap { message in
            guard let payload = Data(base64Encoded: message.payloadB64) else { return nil }
            return NseFetchedRow(id: message.id, payload: payload)
        }
    }

    /// Lowercase-hex encode a byte buffer (matches ``DsClient/pseudonymHex(_:)``).
    private static func hexString(_ bytes: Data) -> String {
        let alphabet = Array("0123456789abcdef")
        var out = String()
        out.reserveCapacity(bytes.count * 2)
        for b in bytes {
            out.append(alphabet[Int(b >> 4)])
            out.append(alphabet[Int(b & 0x0F)])
        }
        return out
    }

    private struct FetchWireResponse: Decodable {
        let messages: [FetchWireMessage]

        enum CodingKeys: String, CodingKey {
            case messages
        }
    }

    private struct FetchWireMessage: Decodable {
        let id: Int64
        let payloadB64: String

        enum CodingKeys: String, CodingKey {
            case id
            case payloadB64 = "payload_b64"
        }
    }

    enum NseDsFetchError: Error {
        case badURL
        case http
    }
}

/// NSE-side configuration. The DS base URL mirrors the app's ``DsClient``
/// default (`http://localhost:8082`) — the whole iOS app is dev-configured
/// today — and is overridable per build via the NSE bundle's `LakkoDsUrl`
/// Info.plist key so a deployment can point it at the real DS without a code
/// change.
enum NseConfig {
    static var dsBaseURL: String {
        if let raw = Bundle.main.object(forInfoDictionaryKey: "LakkoDsUrl") as? String {
            let trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
            if !trimmed.isEmpty { return trimmed }
        }
        return "http://localhost:8082"
    }
}
