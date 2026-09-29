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

    /// The URL fragment (the link secret) that came with ``pendingSlug``, or
    /// `nil` when the link had none. Since the sealed-links amendment of
    /// 2026-09-28 every kind needs it: without it the capability does not
    /// open and the landing shows the damaged state.
    public private(set) var pendingFragment: String?

    /// ADR-0028 (G-3) — the slug from the most recent
    /// `lakko.me/g/<slug>` group join-ticket link tap. `nil` when no
    /// group-join is pending. Drives ``JoinGroupLandingScreen``.
    @Published public private(set) var pendingGroupJoinSlug: String?

    /// ADR-0028 v2 - the URL fragment that came with ``pendingGroupJoinSlug``
    /// (the link secret the capability is sealed under, base64url), or `nil`
    /// when the link had none. Kept beside the slug rather than inside it because the slug is
    /// what the ledger and the parsers see and the fragment must reach neither
    /// - it is decoded on the device and nowhere else.
    public private(set) var pendingGroupJoinFragment: String?

    /// ADR-0032 (G-21/G-22) — the slug from the most recent
    /// `lakko.me/c/<slug>` multi-joiner call-invite link tap. `nil` when no
    /// call-join is pending. Drives ``JoinCallLandingScreen``.
    @Published public private(set) var pendingCallJoinSlug: String?

    /// The URL fragment (the link secret) that came with
    /// ``pendingCallJoinSlug``, or `nil` when the link had none.
    public private(set) var pendingCallJoinFragment: String?

    /// ADR-0032 — set when a guest signup is kicked off from the `/c/` landing,
    /// so that once the session lands and the landing re-mounts, the join
    /// resumes automatically (mirror of the web `pendingJoin` + resume effect).
    /// Consumed one-shot by ``consumeCallJoinAutoResume()``.
    public private(set) var callJoinAutoResume = false

    public init() {}

    /// Extract the slug from a URL and stash it. No-op if the URL
    /// doesn't match the `https://lakko.me/i/<slug>` (contact invite)
    /// or `https://lakko.me/g/<slug>` (group join-ticket, ADR-0028)
    /// shape.
    ///
    /// We accept any URL whose path starts with `/i/` or `/g/`; the
    /// host match is left loose (`lakko.me`, `lakko.app`, dev sub-
    /// domains) so a future deployment rename doesn't require a
    /// coordinated client release. The `apple-app-site-association`
    /// JSON on the server is the security gate — iOS only invokes
    /// `.onOpenURL` for URLs whose host has a verified AASA file
    /// claiming this bundle id.
    public func handle(_ url: URL) {
        let path = url.path
        // Every fragment is set BEFORE its published slug so an observer that
        // wakes on the slug already finds the secret.
        if let slug = Self.slug(from: path, prefix: "/i/") {
            pendingFragment = Self.fragment(of: url)
            pendingSlug = slug
        } else if let slug = Self.slug(from: path, prefix: "/g/") {
            // The fragment is set BEFORE the published slug so an observer
            // that wakes on the slug already finds its key.
            pendingGroupJoinFragment = Self.fragment(of: url)
            pendingGroupJoinSlug = slug
        } else if let slug = Self.slug(from: path, prefix: "/c/") {
            pendingCallJoinFragment = Self.fragment(of: url)
            pendingCallJoinSlug = slug
        }
    }

    /// Extract + trim the slug after `prefix`, or `nil` if the path
    /// doesn't start with `prefix` / the slug is empty. Strips any
    /// trailing slash so `…/<slug>/` works the same as `…/<slug>`.
    /// Internal rather than private since the Calls screen's "Join by link"
    /// validates a PASTED link with the same rule a TAPPED one goes through —
    /// two spellings of "is this a call link" is how the two paths start
    /// disagreeing about the same string.
    static func slug(from path: String, prefix: String) -> String? {
        guard path.hasPrefix(prefix) else { return nil }
        let trimmed = String(path.dropFirst(prefix.count))
            .trimmingCharacters(in: CharacterSet(charactersIn: "/"))
        return trimmed.isEmpty ? nil : trimmed
    }

    /// The raw URL fragment (nothing after `#` is percent-decoded by
    /// `URL.fragment`, and base64url never needs it), `nil` when absent or
    /// empty.
    static func fragment(of url: URL) -> String? {
        guard let fragment = url.fragment, !fragment.isEmpty else { return nil }
        return fragment
    }

    /// Consume the pending contact-invite slug — called by
    /// ``InviteLandingScreen`` once the result has been surfaced to
    /// the user (and the user has acknowledged it with Done / Open
    /// chat / etc.).
    public func clear() {
        pendingSlug = nil
        pendingFragment = nil
    }

    /// ADR-0028 (G-3) — consume the pending group-join slug, called by
    /// ``JoinGroupLandingScreen`` on dismiss.
    public func clearGroupJoin() {
        pendingGroupJoinSlug = nil
        pendingGroupJoinFragment = nil
    }

    /// ADR-0032 — consume the pending call-join slug + auto-resume flag, called
    /// by ``JoinCallLandingScreen`` on dismiss.
    public func clearCallJoin() {
        pendingCallJoinSlug = nil
        pendingCallJoinFragment = nil
        callJoinAutoResume = false
    }

    /// ADR-0032 — park a call link again (the "Open" of a running link call's
    /// status line), exactly as if its URL had just been tapped.
    public func parkCallJoin(slug: String, fragment: String?) {
        pendingCallJoinFragment = fragment
        pendingCallJoinSlug = slug
    }

    /// ADR-0032 — arm the auto-resume flag before a guest signup, so the
    /// re-mounted landing continues the join once the session lands.
    public func armCallJoinAutoResume() {
        callJoinAutoResume = true
    }

    /// ADR-0032 — read + clear the auto-resume flag (one-shot).
    public func consumeCallJoinAutoResume() -> Bool {
        let value = callJoinAutoResume
        callJoinAutoResume = false
        return value
    }
}
