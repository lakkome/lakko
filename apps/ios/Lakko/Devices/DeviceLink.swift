// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

// ADR-0049 section 2, as amended 2026-09-28 — the device link: the camera-less
// "confirm on another device" route (`docs/ux/identity-recovery.md` section 4).
//
// The signed-in (OLD) device chooses a six-digit code and shows it; the person
// types it on the NEW device; the two devices agree on a key from it with
// SPAKE2 (`DeviceLinkInitiator` / `DeviceLinkResponder` in the Rust core), so
// a relay - or the auth server itself - gets ONE guess per request. The new
// device's pairing payload (the bytes the QR would carry) travels sealed under
// that key; the old device pairs only when the new device's confirmation
// verifies, and the new device trusts the Welcome only after the old device's
// confirmation verifies.
//
// This file holds the HTTP relay (``DeviceLinkClient``), the old device's
// daily answer limit, and the two flows as plain state machines with injected
// ports, so every step is driven by a test with fakes.

// MARK: - HTTP relay

/// What `POST /device/request/status` says about a request.
public struct DeviceLinkStatus: Equatable {
    /// The request exists and has not expired. `false` after a decline or an
    /// expiry (the server does not tell the two apart).
    public var pending: Bool
    /// The old device's SPAKE2 message, once posted.
    public var pakeB: Data?
    /// `"accepted"` or `"wrong_code"` once the old device decided.
    public var outcome: String?
    /// The old device's key confirmation when accepted.
    public var confirmB: Data?

    public init(pending: Bool, pakeB: Data? = nil, outcome: String? = nil, confirmB: Data? = nil) {
        self.pending = pending
        self.pakeB = pakeB
        self.outcome = outcome
        self.confirmB = confirmB
    }
}

/// The open request as the old device reads it (`GET /device/requests`).
public struct DeviceLinkPendingRequest: Equatable {
    public var requestIdB64: String
    public var pakeB: Data?
    public var pakeA: Data?
    public var sealedPayload: Data?
    public var confirmA: Data?
    public var outcome: String?

    public init(
        requestIdB64: String, pakeB: Data? = nil, pakeA: Data? = nil,
        sealedPayload: Data? = nil, confirmA: Data? = nil, outcome: String? = nil
    ) {
        self.requestIdB64 = requestIdB64
        self.pakeB = pakeB
        self.pakeA = pakeA
        self.sealedPayload = sealedPayload
        self.confirmA = confirmA
        self.outcome = outcome
    }
}

/// What posting the old device's answer did.
public enum DeviceLinkAnswerResult: Equatable {
    /// `200`: this device answers the request; show the code now.
    case answered
    /// `409 device-request-already-answered`: another device of the account
    /// answered first.
    case handledElsewhere
    /// `404`: the request is gone (declined, replaced or expired).
    case gone
}

/// The relay calls both flows make. ``DeviceLinkClient`` in production.
public protocol DeviceLinkRelay: AnyObject {
    // New device (anonymous).
    func createRequest(handle: Data) async throws -> String
    func status(requestIdB64: String) async throws -> DeviceLinkStatus
    func postPayload(requestIdB64: String, sealed: DeviceLinkSealed) async throws
    // Old device (bearer).
    func openRequest(sessionToken: String) async throws -> DeviceLinkPendingRequest?
    func answer(requestIdB64: String, pakeB: Data, sessionToken: String) async throws -> DeviceLinkAnswerResult
    func postResult(requestIdB64: String, confirmB: Data?, sessionToken: String) async throws
    func decline(requestIdB64: String, sessionToken: String) async throws
}

/// `services/auth` device-request routes over the ambient transport.
/// Failures surface as ``AuthClientError`` so the rate-limit copy
/// (``AuthRateLimit``) applies unchanged.
public final class DeviceLinkClient: DeviceLinkRelay {
    private let baseURL: String
    private let session: URLSession

    public init(baseURL: String = AuthClient.defaultBaseURL, session: URLSession = .shared) {
        self.baseURL = baseURL
        self.session = session
    }

    public func createRequest(handle: Data) async throws -> String {
        let (data, _) = try await call(
            "POST", "/device/request", body: ["handle": base64UrlEncodeNoPad(handle)])
        guard let id = (try? JSONSerialization.jsonObject(with: data) as? [String: Any])?[
            "request_id_b64"] as? String else {
            throw AuthClientError.http(status: 200, endpoint: "/device/request", body: "no request id")
        }
        return id
    }

    public func status(requestIdB64: String) async throws -> DeviceLinkStatus {
        let (data, _) = try await call(
            "POST", "/device/request/status", body: ["request_id_b64": requestIdB64])
        let object = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any] ?? [:]
        return DeviceLinkStatus(
            pending: object["pending"] as? Bool ?? false,
            pakeB: (object["pake_b_b64"] as? String).flatMap { Data(base64Encoded: $0) },
            outcome: object["outcome"] as? String,
            confirmB: (object["confirm_b_b64"] as? String).flatMap { Data(base64Encoded: $0) })
    }

    public func postPayload(requestIdB64: String, sealed: DeviceLinkSealed) async throws {
        _ = try await call("POST", "/device/request/payload", body: [
            "request_id_b64": requestIdB64,
            "pake_a_b64": sealed.pakeA.base64EncodedString(),
            "sealed_payload_b64": sealed.sealedPayload.base64EncodedString(),
            "confirm_a_b64": sealed.confirmA.base64EncodedString(),
        ])
    }

    public func openRequest(sessionToken: String) async throws -> DeviceLinkPendingRequest? {
        let (data, _) = try await call("GET", "/device/requests", bearer: sessionToken)
        let object = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any] ?? [:]
        guard let request = object["request"] as? [String: Any],
              let id = request["request_id_b64"] as? String else { return nil }
        func blob(_ key: String) -> Data? { (request[key] as? String).flatMap { Data(base64Encoded: $0) } }
        return DeviceLinkPendingRequest(
            requestIdB64: id,
            pakeB: blob("pake_b_b64"),
            pakeA: blob("pake_a_b64"),
            sealedPayload: blob("sealed_payload_b64"),
            confirmA: blob("confirm_a_b64"),
            outcome: request["outcome"] as? String)
    }

    public func answer(requestIdB64: String, pakeB: Data, sessionToken: String) async throws
        -> DeviceLinkAnswerResult {
        do {
            _ = try await call(
                "POST", "/device/requests/\(requestIdB64)/pake",
                body: ["pake_b_b64": pakeB.base64EncodedString()], bearer: sessionToken)
            return .answered
        } catch AuthClientError.http(let status, _, _) where status == 409 {
            return .handledElsewhere
        } catch AuthClientError.http(let status, _, _) where status == 404 {
            return .gone
        }
    }

    public func postResult(requestIdB64: String, confirmB: Data?, sessionToken: String) async throws {
        var body: [String: String] = ["outcome": confirmB == nil ? "wrong_code" : "accepted"]
        if let confirmB { body["confirm_b_b64"] = confirmB.base64EncodedString() }
        _ = try await call(
            "POST", "/device/requests/\(requestIdB64)/result", body: body, bearer: sessionToken)
    }

    public func decline(requestIdB64: String, sessionToken: String) async throws {
        _ = try await call("DELETE", "/device/requests/\(requestIdB64)", bearer: sessionToken)
    }

    private func call(
        _ method: String, _ endpoint: String, body: [String: String]? = nil, bearer: String? = nil
    ) async throws -> (Data, HTTPURLResponse) {
        guard let url = URL(string: baseURL + endpoint) else {
            throw AuthClientError.argument("invalid endpoint \(endpoint)")
        }
        var request = URLRequest(url: url)
        request.httpMethod = method
        if let body {
            request.setValue("application/json", forHTTPHeaderField: "Content-Type")
            request.httpBody = try JSONSerialization.data(withJSONObject: body)
        }
        if let bearer { request.setValue("Bearer \(bearer)", forHTTPHeaderField: "Authorization") }
        let raw: (Data, URLResponse)
        do {
            raw = try await AntiCensorshipTransport.perform(request, session: session, config: .current)
        } catch {
            throw AuthClientError.network(endpoint: endpoint, underlying: error)
        }
        guard let http = raw.1 as? HTTPURLResponse else {
            throw AuthClientError.http(status: 0, endpoint: endpoint, body: "non-HTTP response")
        }
        guard (200..<300).contains(http.statusCode) else {
            let text = String(data: raw.0, encoding: .utf8) ?? ""
            throw AuthClientError.http(status: http.statusCode, endpoint: endpoint, body: String(text.prefix(512)))
        }
        return (raw.0, http)
    }
}

// MARK: - Answer limit (old device)

/// An old device answers at most ``maxAnswers`` requests per rolling 24 hours
/// (a chosen limit, not a measured one), persisted per device. Over it the
/// modal says so and the request is declined, so a stream of requests cannot
/// keep putting a code in front of the person.
public final class DeviceLinkAnswerLimit {
    public static let maxAnswers = 5
    public static let window: TimeInterval = 24 * 60 * 60
    public static let copy = "Too many sign-in requests today. Use a QR code, or try again tomorrow."

    private let defaults: UserDefaults
    private let key: String

    public init(defaults: UserDefaults = .standard, key: String = "me.lakko.device-link.answers.v1") {
        self.defaults = defaults
        self.key = key
    }

    private func recent(now: Date) -> [Double] {
        let all = defaults.array(forKey: key) as? [Double] ?? []
        return all.filter { now.timeIntervalSince1970 - $0 < Self.window && $0 <= now.timeIntervalSince1970 }
    }

    /// Whether one more answer is allowed at `now`.
    public func canAnswer(now: Date = Date()) -> Bool {
        recent(now: now).count < Self.maxAnswers
    }

    /// Record one answer.
    public func record(now: Date = Date()) {
        var list = recent(now: now)
        list.append(now.timeIntervalSince1970)
        defaults.set(list, forKey: key)
    }
}

// MARK: - New device flow

/// The copy of the new device's screen (`docs/ux/identity-recovery.md` 4).
public enum DeviceLinkJoinCopy {
    public static let waiting = "Waiting for your other device..."
    public static let wrongCode =
        "That code does not match. Start again and enter the code your other device shows."
    public static let declined = "Declined on another device."
    public static let expired = "This request expired. Start again."
    public static let restoring = "Restoring..."
}

/// The new device's side, as a state machine over injected ports.
@MainActor
public final class DeviceLinkJoin: ObservableObject {
    public enum Phase: Equatable {
        case idle
        /// Posted; the field is disabled until the other device answers.
        case waiting
        /// The other device answered; the six-digit field is enabled.
        case enterCode
        /// Continue pressed; the exchange ran and the result is awaited.
        case checking
        case wrongCode
        /// The other device accepted and its confirmation VERIFIED.
        case confirmed
        case declined
        case expired
        case failed(String)
    }

    public struct Ports {
        public var relay: DeviceLinkRelay
        /// `DeviceLinkInitiator(requestId:loginHandle:)`.
        public var makeInitiator: (_ requestId: Data, _ handle: Data) throws -> DeviceLinkInitiatorProtocol
        public var sleep: (_ seconds: Double) async -> Void
        public var now: () -> Date

        public init(
            relay: DeviceLinkRelay,
            makeInitiator: @escaping (Data, Data) throws -> DeviceLinkInitiatorProtocol = {
                try DeviceLinkInitiator(requestId: $0, loginHandle: $1)
            },
            sleep: @escaping (Double) async -> Void = { try? await Task.sleep(nanoseconds: UInt64($0 * 1_000_000_000)) },
            now: @escaping () -> Date = { Date() }
        ) {
            self.relay = relay
            self.makeInitiator = makeInitiator
            self.sleep = sleep
            self.now = now
        }
    }

    /// Ten minutes: the server's request lifetime.
    public static let lifetime: TimeInterval = 600
    public static let pollInterval: Double = 2

    @Published public private(set) var phase: Phase = .idle

    private let ports: Ports
    private let handle: Data
    /// The pairing payload the QR would carry (`buildPairingQr`).
    private let pairingPayload: Data
    private var requestIdB64: String?
    private var initiator: DeviceLinkInitiatorProtocol?
    private var pakeB: Data?
    private var startedAt: Date?

    public init(ports: Ports, handle: Data, pairingPayload: Data) {
        self.ports = ports
        self.handle = handle
        self.pairingPayload = pairingPayload
    }

    /// Post a request and wait for the other device's answer.
    public func start() async {
        phase = .waiting
        pakeB = nil
        do {
            let id = try await ports.relay.createRequest(handle: handle)
            guard let idBytes = base64UrlDecodeNoPad(id) else {
                phase = .failed("The server answered in an unexpected way. Try again.")
                return
            }
            requestIdB64 = id
            initiator = try ports.makeInitiator(idBytes, handle)
            startedAt = ports.now()
        } catch let error as AuthClientError {
            phase = .failed(AuthRateLimit.copy(for: error) ?? "Could not reach the server. Try again.")
            return
        } catch {
            phase = .failed("Could not reach the server. Try again.")
            return
        }
        await pollUntil { status in
            if let pakeB = status.pakeB {
                self.pakeB = pakeB
                self.phase = .enterCode
                return true
            }
            return false
        }
        // Keep watching while the person types, so a decline ("It is not me")
        // or the expiry shows without waiting for Continue. Ends as soon as
        // Continue moves the phase on.
        guard phase == .enterCode else { return }
        await pollUntil { _ in self.phase != .enterCode }
    }

    /// Continue with the typed code: run the exchange, post the sealed
    /// payload, and wait for the other device's decision. Returns `true` only
    /// when the other device accepted AND its confirmation verified, which is
    /// the one condition under which the caller may trust the Welcome.
    @discardableResult
    public func submit(code typed: String) async -> Bool {
        guard phase == .enterCode, let initiator, let pakeB, let requestIdB64 else { return false }
        let sealed: DeviceLinkSealed
        do {
            sealed = try initiator.finish(pakeB: pakeB, typedCode: typed, pairingPayload: pairingPayload)
        } catch {
            // Not six digits: nothing was spent, the person may retype.
            return false
        }
        phase = .checking
        do {
            try await ports.relay.postPayload(requestIdB64: requestIdB64, sealed: sealed)
        } catch {
            phase = .failed("Could not reach the server. Try again.")
            return false
        }
        var verified = false
        await pollUntil { status in
            guard let outcome = status.outcome else { return false }
            if outcome == "accepted", let confirmB = status.confirmB {
                do {
                    try initiator.verifyConfirmation(confirmB: confirmB)
                    verified = true
                    self.phase = .confirmed
                } catch {
                    // Whoever answered did not hold the code: trust nothing.
                    self.phase = .wrongCode
                }
            } else {
                self.phase = .wrongCode
            }
            return true
        }
        return verified
    }

    /// A decided phase is never overwritten by a later poll.
    private var isSettled: Bool {
        switch phase {
        case .confirmed, .wrongCode, .declined, .expired, .failed: return true
        default: return false
        }
    }

    /// Poll the status until `done` says so, the request disappears
    /// (declined) or the lifetime runs out (expired).
    private func pollUntil(_ done: @escaping (DeviceLinkStatus) -> Bool) async {
        guard let requestIdB64 else { return }
        while !Task.isCancelled {
            if let startedAt, ports.now().timeIntervalSince(startedAt) > Self.lifetime {
                if !isSettled { phase = .expired }
                return
            }
            if let status = try? await ports.relay.status(requestIdB64: requestIdB64) {
                if done(status) { return }
                if !status.pending {
                    if !isSettled { phase = .declined }
                    return
                }
            }
            await ports.sleep(Self.pollInterval)
        }
    }
}

// MARK: - Old device flow

/// The copy of the old device's modal (`docs/ux/identity-recovery.md` 4).
public enum DeviceLinkModalCopy {
    public static let title = "New sign-in request"
    /// Step 1 (second security pass, finding 6): nothing is sent for the
    /// request until the person says it is them.
    public static let askBody = "Someone is signing in to your account on a new device.\nIs this you?"
    public static let yes = "Yes, it's me"
    /// Step 2, after "Yes, it's me".
    public static let body = "Enter this code on the new device:"
    public static let warning = "Never read this code to anyone. Lakko will never ask for it."
    public static let notMe = "It is not me"
    public static let adding = "Adding the device..."
    public static let wrongCode = "The code did not match. Nothing was added."
    public static let declined = "Declined. If this keeps happening, change your password."
    public static let handledElsewhere = "This request was handled on another device."
    public static let failed = "The device could not be added. Try again from Devices."
}

/// The old device's side: find an open request, answer it, show the code,
/// and act on the new device's sealed payload.
@MainActor
public final class DeviceLinkResponderFlow: ObservableObject {
    public enum Modal: Equatable {
        /// Step 1: "Is this you?". Nothing is sent for the request yet: no
        /// responder exists and no `pake_b` is posted until "Yes, it's me".
        case confirm
        /// Showing the code (`47 19 03`); the request is answered.
        case code(String)
        case adding
        case wrongCode
        case declined
        case handledElsewhere
        case tooMany
        case failed
    }

    public struct Ports {
        public var relay: DeviceLinkRelay
        public var sessionToken: @MainActor () -> String?
        /// This account's own login handle (the payload must name it).
        public var ownHandle: @MainActor () -> Data?
        public var makeResponder: (_ requestId: Data, _ handle: Data) throws -> DeviceLinkResponderProtocol
        /// Parse the opened pairing payload against the own handle; throws on
        /// a payload that names another account.
        public var checkPayload: (_ payload: Data, _ handle: Data) throws -> Void
        /// Run the existing QR pairing with the payload (DeviceGroup add +
        /// recovery row + the chat fan-out).
        public var runPairing: @MainActor (_ payload: Data) async throws -> Void
        public var limit: DeviceLinkAnswerLimit

        public init(
            relay: DeviceLinkRelay,
            sessionToken: @escaping @MainActor () -> String?,
            ownHandle: @escaping @MainActor () -> Data?,
            makeResponder: @escaping (Data, Data) throws -> DeviceLinkResponderProtocol = {
                try DeviceLinkResponder(requestId: $0, loginHandle: $1)
            },
            checkPayload: @escaping (Data, Data) throws -> Void,
            runPairing: @escaping @MainActor (Data) async throws -> Void,
            limit: DeviceLinkAnswerLimit = DeviceLinkAnswerLimit()
        ) {
            self.relay = relay
            self.sessionToken = sessionToken
            self.ownHandle = ownHandle
            self.makeResponder = makeResponder
            self.checkPayload = checkPayload
            self.runPairing = runPairing
            self.limit = limit
        }
    }

    @Published public private(set) var modal: Modal?

    private let ports: Ports
    /// Kept in memory only: if it is lost (a relaunch), the request can only
    /// be declined.
    private var responder: DeviceLinkResponderProtocol?
    private var requestIdB64: String?
    /// The request the "Is this you?" step is showing, not answered yet.
    private var askingRequestIdB64: String?
    /// A request whose question was dismissed without a choice: left
    /// unanswered (it expires), not asked again.
    private var dismissedRequestIdB64: String?
    private var acting = false

    public init(ports: Ports) {
        self.ports = ports
    }

    /// One check: called when the app comes to the foreground and on a timer
    /// while the modal is up.
    public func check(now: Date = Date()) async {
        guard !acting, let token = ports.sessionToken(), let handle = ports.ownHandle() else { return }
        acting = true
        defer { acting = false }
        guard let request = try? await ports.relay.openRequest(sessionToken: token) else {
            // Gone while asking or showing the code: declined elsewhere or
            // expired.
            if case .code = modal { modal = nil }
            if modal == .confirm { modal = nil }
            responder = nil
            requestIdB64 = nil
            askingRequestIdB64 = nil
            return
        }
        if request.requestIdB64 != requestIdB64 {
            if request.requestIdB64 == askingRequestIdB64 {
                // Still asking. Answered on another device meanwhile: close.
                if request.pakeB != nil {
                    askingRequestIdB64 = nil
                    modal = .handledElsewhere
                }
                return
            }
            // A new request (a newer one replaces an older one).
            responder = nil
            requestIdB64 = nil
            askingRequestIdB64 = nil
            guard request.pakeB == nil else {
                // Answered already, by another device or by this one before a
                // relaunch lost the responder: nothing to show.
                if modal == .confirm { modal = nil }
                return
            }
            guard request.requestIdB64 != dismissedRequestIdB64 else { return }
            // Second security pass, finding 6 — ask first. No responder, no
            // `pake_b`, nothing sent until "Yes, it's me".
            askingRequestIdB64 = request.requestIdB64
            modal = .confirm
            return
        }
        // Our request: act on the new device's payload once it is there.
        guard let responder, request.outcome == nil,
              let pakeA = request.pakeA, let sealed = request.sealedPayload,
              let confirmA = request.confirmA else { return }
        let id = request.requestIdB64
        self.responder = nil
        let accepted: DeviceLinkAccepted
        do {
            accepted = try responder.accept(pakeA: pakeA, sealedPayload: sealed, confirmA: confirmA)
            try ports.checkPayload(accepted.pairingPayload, handle)
        } catch {
            try? await ports.relay.postResult(requestIdB64: id, confirmB: nil, sessionToken: token)
            modal = .wrongCode
            return
        }
        modal = .adding
        do {
            try await ports.relay.postResult(requestIdB64: id, confirmB: accepted.confirmB, sessionToken: token)
            try await ports.runPairing(accepted.pairingPayload)
            modal = nil
        } catch {
            modal = .failed
        }
    }

    /// "Yes, it's me": only now is the responder created and `pake_b`
    /// posted, and the answer counted against the daily limit (the limit
    /// counts answers after "Yes").
    public func yesItsMe(now: Date = Date()) async {
        guard modal == .confirm, !acting, let id = askingRequestIdB64,
              let token = ports.sessionToken(), let handle = ports.ownHandle() else { return }
        acting = true
        defer { acting = false }
        askingRequestIdB64 = nil
        guard ports.limit.canAnswer(now: now) else {
            modal = .tooMany
            try? await ports.relay.decline(requestIdB64: id, sessionToken: token)
            return
        }
        guard let idBytes = base64UrlDecodeNoPad(id),
              let fresh = try? ports.makeResponder(idBytes, handle) else {
            modal = .failed
            return
        }
        let result: DeviceLinkAnswerResult
        do {
            result = try await ports.relay.answer(
                requestIdB64: id, pakeB: fresh.pakeMessage(), sessionToken: token)
        } catch {
            modal = .failed
            return
        }
        switch result {
        case .answered:
            ports.limit.record(now: now)
            responder = fresh
            requestIdB64 = id
            let code = fresh.code()
            modal = .code((try? deviceLinkFormatCode(code: code)) ?? code)
        case .handledElsewhere:
            modal = .handledElsewhere
        case .gone:
            modal = nil
        }
    }

    /// "It is not me": decline the request, whether it was only asked about
    /// or already answered.
    public func notMe() async {
        guard let id = requestIdB64 ?? askingRequestIdB64, let token = ports.sessionToken() else { return }
        responder = nil
        askingRequestIdB64 = nil
        try? await ports.relay.decline(requestIdB64: id, sessionToken: token)
        modal = .declined
    }

    /// Close the modal. The code step cannot be closed; closing the question
    /// without a choice sends nothing and leaves the request unanswered (it
    /// expires), and it is not asked again.
    public func dismiss() {
        if case .code = modal { return }
        if modal == .confirm {
            dismissedRequestIdB64 = askingRequestIdB64
            askingRequestIdB64 = nil
        }
        modal = nil
    }
}
