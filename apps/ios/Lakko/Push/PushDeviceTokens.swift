// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// This device's provider tokens (ADR-0078 section 3).
///
/// Before ADR-0078 the device registered each token once at push-gw under a
/// random device pseudonym and subscribed that pseudonym to every group, so
/// push-gw held the devices x groups graph. Now push-gw holds no pseudonym and
/// no token in the clear: every group subscription carries this device's
/// tokens sealed under that group's unlock key
/// (``PushGroupRouting/subscribeUnderPushGidBestEffort(accountIdB64:groupIdHex:)``).
/// So the tokens are kept here, on the device, and sealed afresh into each
/// subscription; when one changes, every subscription of the signed-in
/// account is sent again.
///
/// An iOS device holds up to two: the **alert** token
/// (`registerForRemoteNotifications`) and the **call** token (PushKit). Both
/// ride one subscription, because push-gw's delivery rule (a call goes to the
/// call token when there is one, a message never does) needs the device's
/// whole set.
///
/// Device state, not account state: kept in the app's own `UserDefaults`
/// (never the App Group: the extension does not need it). Cleared at Log out
/// and Forget device after the account's subscriptions were removed.
@MainActor
public final class PushDeviceTokens {

    public static let shared = PushDeviceTokens()

    private static let alertKey = "me.lakko.push-token.alert.v1"
    private static let voipKey = "me.lakko.push-token.voip.v1"

    /// Longest token a subscription carries (mirror of `MAX_PUSH_TOKEN_LEN`).
    public static let maxTokenLength = 2048

    private let defaults: UserDefaults

    public init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }

    /// The alert token, or `nil`.
    public var alert: Data? { read(Self.alertKey) }

    /// The call token, or `nil`.
    public var voip: Data? { read(Self.voipKey) }

    /// Store the alert token. Returns whether it changed.
    @discardableResult
    public func setAlert(_ token: Data?) -> Bool { write(token, Self.alertKey) }

    /// Store the call token. Returns whether it changed.
    @discardableResult
    public func setVoip(_ token: Data?) -> Bool { write(token, Self.voipKey) }

    /// The entries a sealed subscription carries; empty when this device has
    /// no token (push off): then nothing is subscribed.
    public func entries() -> [FfiPushTokenEntry] {
        var out: [FfiPushTokenEntry] = []
        if let alert { out.append(FfiPushTokenEntry(provider: .apns, token: alert)) }
        if let voip { out.append(FfiPushTokenEntry(provider: .apnsVoip, token: voip)) }
        return out
    }

    /// Forget both tokens.
    public func clear() {
        defaults.removeObject(forKey: Self.alertKey)
        defaults.removeObject(forKey: Self.voipKey)
    }

    private func read(_ key: String) -> Data? {
        guard let data = defaults.data(forKey: key), !data.isEmpty,
              data.count <= Self.maxTokenLength else { return nil }
        return data
    }

    private func write(_ token: Data?, _ key: String) -> Bool {
        let next = token.flatMap { $0.isEmpty || $0.count > Self.maxTokenLength ? nil : $0 }
        let before = read(key)
        if let next {
            defaults.set(next, forKey: key)
        } else {
            defaults.removeObject(forKey: key)
        }
        return before != next
    }
}
