// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// The two APNs tokens an iOS Lakko device holds, and which push-gateway
/// provider each is registered under.
///
/// ## Why there are two
///
/// They are different tokens for different push types, issued by different
/// system facilities, and one cannot deliver the other's pushes:
///
///   * The **alert** token comes from
///     `UIApplication.registerForRemoteNotifications()` and receives the
///     mutable-alert message wakes the notification-service extension decrypts.
///   * The **VoIP** token comes from `PKPushRegistry` and receives
///     `apns-push-type: voip` pushes, the only push that can launch a
///     terminated app and let it report a call to CallKit
///     (`docs/ux/calls.md` section 2.1).
///
/// Registering one in the other's slot does not degrade — it silently breaks
/// that class of push, which is why the kind is a type rather than a `Bool`
/// somewhere.
public enum PushTokenKind: String, Equatable, CaseIterable, Sendable {
    case alert
    case voip

    /// The `services/push-gw` provider this token is registered under. The two
    /// are distinct rows for one device, so a fresh token of one kind never
    /// overwrites the other's.
    public var provider: PushClient.Provider {
        switch self {
        case .alert: return .apns
        case .voip: return .apnsVoip
        }
    }
}

/// When an arriving token is worth sending to the gateway.
public enum PushTokenRegistration {

    /// `true` when this token should be uploaded.
    ///
    /// iOS re-delivers the same token on every launch, and a registration costs
    /// an anonymous token plus a round trip, so an unchanged token is
    /// skipped. An empty token is never sent: the gateway rejects it, and a
    /// device that reports one has no working push either way — the failure
    /// belongs in a log, not in a request.
    public static func shouldRegister(token: Data, lastRegistered: Data?) -> Bool {
        guard !token.isEmpty else { return false }
        return token != lastRegistered
    }

    /// Lowercase-hex form of a token, as APNs and every provider console shows
    /// it. Used for logging a token's identity without logging its bytes
    /// verbatim in some other shape.
    public static func hex(_ token: Data) -> String {
        let alphabet = Array("0123456789abcdef")
        var out = String()
        out.reserveCapacity(token.count * 2)
        for byte in token {
            out.append(alphabet[Int(byte >> 4)])
            out.append(alphabet[Int(byte & 0x0F)])
        }
        return out
    }
}
