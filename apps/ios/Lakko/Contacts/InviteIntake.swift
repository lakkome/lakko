// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// Process-scoped holder for a pending invite-link slug.
///
/// Mirror of:
///
///   * `apps/android/.../contacts/InviteIntake.kt` (1.15e11a)
///   * `apps/web/src/contacts/InviteIntake.ts` (1.17e11a)
///
/// When the user taps a `https://lakko.me/i/<slug>` Universal Link:
///
///   1. iOS launches the app (cold-start) or hands the URL to a
///      running instance via the `UIScene` deep-link path.
///   2. SwiftUI's `.onOpenURL { url in ... }` modifier on the
///      root view scope fires.
///   3. We extract the slug from the URL path and stash it here.
///   4. ``AppRoot`` observes ``pendingSlug``; when non-nil AND the
///      auth state is `.session`, the router swaps to the
///      ``InviteLandingScreen`` which drives the consume flow.
///   5. The landing screen calls ``clear()`` once the slug has
///      been consumed (or the user dismissed the result), so a
///      subsequent in-session intent doesn't replay the same
///      invite.
///
/// **Cold-start vs hot deep link**: SwiftUI's `.onOpenURL` fires
/// for both cases on iOS 14+. The handler runs before the first
/// `body` evaluation on cold start — by the time AppRoot reads
/// `pendingSlug`, it's already populated.
///
/// **Anon-state intents**: if the user isn't signed in, the slug
/// stays parked here until they finish auth. The first session-
/// state transition (loading → session) flips the AppRoot router
/// to InviteLandingScreen, which then drives the consume. This
/// matches the Android `MainActivity.intentSlug + LaunchedEffect`
/// pattern.
@MainActor
public final class InviteIntake: ObservableObject {

    /// The slug from the most recent `lakko.me/i/<slug>` Universal
    /// Link tap. `nil` when no invite is pending.
    @Published public private(set) var pendingSlug: String?

    public init() {}

    /// Extract the slug from a URL and stash it. No-op if the URL
    /// doesn't match the `https://lakko.me/i/<slug>` shape.
    ///
    /// We accept any URL whose path starts with `/i/`; the host
    /// match is left loose (`lakko.me`, `lakko.app`, dev sub-
    /// domains) so a future deployment rename doesn't require a
    /// coordinated client release. The `apple-app-site-association`
    /// JSON on the server is the security gate — iOS only invokes
    /// `.onOpenURL` for URLs whose host has a verified AASA file
    /// claiming this bundle id.
    public func handle(_ url: URL) {
        let path = url.path
        guard path.hasPrefix("/i/") else { return }
        let slug = String(path.dropFirst("/i/".count))
        guard !slug.isEmpty else { return }
        // Strip any trailing slash so `lakko.me/i/<slug>/` works
        // the same as `lakko.me/i/<slug>`.
        let trimmed = slug.trimmingCharacters(in: CharacterSet(charactersIn: "/"))
        guard !trimmed.isEmpty else { return }
        pendingSlug = trimmed
    }

    /// Consume the pending slug — called by ``InviteLandingScreen``
    /// once the result has been surfaced to the user (and the user
    /// has acknowledged it with Done / Open chat / etc.).
    public func clear() {
        pendingSlug = nil
    }
}
