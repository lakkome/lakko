// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// The two APNs tokens an iOS Lakko device holds, and which push provider each
/// is sealed under in a subscription (ADR-0078: both ride every subscription,
/// see `PushDeviceTokens`).
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

    /// The provider this token is sealed under. The two are distinct entries
    /// of one subscription, so a fresh token of one kind never overwrites the
    /// other's.
    public var provider: FfiPushProvider {
        switch self {
        case .alert: return .apns
        case .voip: return .apnsVoip
        }
    }
}
