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

    /// The link token (`slug#fragment`, see ``InviteLinkSecret/token(slug:fragment:)``)
    /// from the most recent `lakko.me/i/<slug>` Universal Link tap. `nil` when
    /// no invite is pending.
    @Published public private(set) var pendingSlug: String?

    /// ADR-0028 (G-3) — the slug from the most recent
    /// `lakko.me/g/<slug>` group join-ticket link tap. `nil` when no
    /// group-join is pending. Drives ``JoinGroupLandingScreen``.
    @Published public private(set) var pendingGroupJoinSlug: String?

    /// ADR-0028 v2 - the URL fragment that came with ``pendingGroupJoinSlug``
    /// (the sealed preview's link key, base64url), or `nil` when the link had
    /// none. Kept beside the slug rather than inside it because the slug is
    /// what the ledger and the parsers see and the fragment must reach neither
    /// - it is decoded on the device and nowhere else.
    public private(set) var pendingGroupJoinFragment: String?

    /// ADR-0032 (G-21/G-22) — the link token (`slug#fragment`) from the most
    /// recent `lakko.me/c/<slug>` multi-joiner call-invite link tap. `nil` when
    /// no call-join is pending. Drives ``JoinCallLandingScreen``.
    @Published public private(set) var pendingCallJoinSlug: String?

    /// ADR-0063 — a `/feed/import#<rule>` link waiting for the Feed tab.
    @Published private(set) var pendingFeedImport: FeedImportRequest?

    /// Take the parked feed import, if any.
    func takeFeedImport() -> FeedImportRequest? {
        let request = pendingFeedImport
        pendingFeedImport = nil
        return request
    }

    /// ADR-0032 — set when a guest signup is kicked off from the `/c/` landing,
    /// so that once the session lands and the landing re-mounts, the join
    /// resumes automatically (mirror of the web `pendingJoin` + resume effect).
    /// Consumed one-shot by ``consumeCallJoinAutoResume()``.
    public private(set) var callJoinAutoResume = false
    /// The name a new guest typed on the `/c/` landing before its account was
    /// created (`docs/ux/call-links.md` section 9.1): the call the re-mounted
    /// landing starts carries it as the call name. Taken one-shot by
    /// ``takeCallJoinGuestName()``.
    private(set) var callJoinGuestName: String?
    /// Numbers every open of a call link, the same link opened again
    /// included: the landing draws a new guest name for each
    /// (`docs/ux/call-links.md` section 9.1). Set BEFORE the slug, so a
    /// landing that wakes on the slug already finds its open.
    @Published private(set) var callJoinOpens: UInt64 = 0

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
        // ADR-0063, `docs/ux/feed.md` section 5 — a `.lakkofeed` file opened
        // from Files, mail or a messenger ("Open in Lakko") lands on the same
        // import screen as a link.
        if url.isFileURL {
            if FeedDraft.isSharedFeedFile(url) {
                pendingFeedImport = FeedDraft.importRequest(fromFile: url)
            }
            return
        }
        // `docs/ux/desktop.md` section 7.1 — "Open in the app" on a link's
        // page in Safari opens `lakko://<kind>/<id>#<secret>` (or
        // `lakko://feed/import#<rule>`): Safari never hands a link to the app
        // while the person stays on the same site. It is read as this build's
        // own `https` link and goes the same way; anything else is ignored.
        if url.scheme?.lowercased() == Self.appScheme {
            if let link = Self.appSchemeLink(url) { handle(link) }
            return
        }
        let path = url.path
        if let slug = Self.slug(from: path, prefix: "/i/") {
            // ADR-0028 amendment 2026-09-28 — the sealed-link secret rides the
            // fragment; it is parked inside the token (`slug#fragment`) so it
            // reaches the landing through the route, never a server.
            pendingSlug = InviteLinkSecret.token(slug: slug, fragment: Self.fragment(of: url))
        } else if let slug = Self.slug(from: path, prefix: "/g/") {
            // The fragment is set BEFORE the published slug so an observer
            // that wakes on the slug already finds its key.
            pendingGroupJoinFragment = Self.fragment(of: url)
            pendingGroupJoinSlug = slug
        } else if let slug = Self.slug(from: path, prefix: "/c/") {
            callJoinOpens &+= 1
            pendingCallJoinSlug = InviteLinkSecret.token(slug: slug, fragment: Self.fragment(of: url))
        } else if path == "/feed/import" || path == "/feed/import/" {
            // ADR-0063 — a shared feed rides in the fragment, never the path.
            let fragment = Self.fragment(of: url)
            let bytes = FeedDraft.bytes(fromFragment: fragment)
            pendingFeedImport = FeedImportRequest(
                bytes: bytes, refused: bytes == nil && fragment != nil)
        }
    }

    /// A scanned QR code's text as a Lakko link the app opens, or `nil`.
    /// Only an `https` link on this build's web origin (or `lakko.me` and
    /// its subdomains) whose path is `/i/`, `/g/` or `/c/` with a slug
    /// counts: unlike a tapped Universal Link, no `apple-app-site-association`
    /// check stands behind a code someone holds up to the camera. The result
    /// goes through ``handle(_:)``, the same route as an opened link.
    nonisolated static func scannedLink(_ text: String, webOrigin: String = ApiEndpoints.webOrigin) -> URL? {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let url = URL(string: trimmed),
              url.scheme?.lowercased() == "https",
              let host = url.host?.lowercased() else { return nil }
        let originHost = URL(string: webOrigin)?.host?.lowercased()
        let hostOk = host == originHost || host == "lakko.me" || host.hasSuffix(".lakko.me")
        guard hostOk else { return nil }
        let path = url.path
        let isLink = ["/i/", "/g/", "/c/"].contains { slug(from: path, prefix: $0) != nil }
        return isLink ? url : nil
    }

    /// The app's own URL scheme, registered in `project.yml`
    /// (`CFBundleURLTypes`), the same scheme the desktop app takes.
    nonisolated static let appScheme = "lakko"

    /// A `lakko://` address as this build's `https` link on `webOrigin`, or
    /// `nil`. Strict, as the desktop host's parse is (`apps/desktop/src-tauri/
    /// src/link.rs`): a ledger link is `lakko://<i|c|g>/<8 base64url>#<16
    /// base64url>`, a shared feed `lakko://feed/import#<base64url>`, with no
    /// query and nothing else in the path. Any page or app can open this
    /// scheme, so nothing here trusts it beyond its shape: the landing that
    /// follows opens the sealed link and asks the person before anything
    /// happens, exactly as for a tapped `https` link.
    nonisolated static func appSchemeLink(_ url: URL, webOrigin: String = ApiEndpoints.webOrigin) -> URL? {
        guard url.scheme?.lowercased() == appScheme,
              url.query == nil, url.user == nil, url.port == nil,
              let kind = url.host,
              let fragment = url.fragment, !fragment.isEmpty,
              fragment.allSatisfy(isBase64UrlCharacter) else { return nil }
        let origin = webOrigin.hasSuffix("/") ? String(webOrigin.dropLast()) : webOrigin
        let path = url.path
        switch kind {
        case "i", "c", "g":
            let id = path.hasPrefix("/") ? String(path.dropFirst()) : ""
            guard id.count == 8, id.allSatisfy(isBase64UrlCharacter),
                  fragment.count == 16 else { return nil }
            return URL(string: "\(origin)/\(kind)/\(id)#\(fragment)")
        case "feed":
            guard path == "/import" else { return nil }
            return URL(string: "\(origin)/feed/import#\(fragment)")
        default:
            return nil
        }
    }

    nonisolated private static func isBase64UrlCharacter(_ character: Character) -> Bool {
        guard character.isASCII, let ascii = character.asciiValue else { return false }
        return (ascii >= 0x30 && ascii <= 0x39) || (ascii >= 0x41 && ascii <= 0x5A)
            || (ascii >= 0x61 && ascii <= 0x7A) || ascii == 0x2D || ascii == 0x5F
    }

    /// Extract + trim the slug after `prefix`, or `nil` if the path
    /// doesn't start with `prefix` / the slug is empty. Strips any
    /// trailing slash so `…/<slug>/` works the same as `…/<slug>`.
    /// Internal rather than private since the Calls screen's "Join by link"
    /// validates a PASTED link with the same rule a TAPPED one goes through —
    /// two spellings of "is this a call link" is how the two paths start
    /// disagreeing about the same string.
    nonisolated static func slug(from path: String, prefix: String) -> String? {
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
        callJoinAutoResume = false
        callJoinGuestName = nil
    }

    /// ADR-0032 — park a call link again (the "Open" of a running link call's
    /// status line), exactly as if its URL had just been tapped.
    public func parkCallJoin(slug: String) {
        callJoinOpens &+= 1
        pendingCallJoinSlug = slug
    }

    /// ADR-0032 — arm the auto-resume flag before a guest signup, so the
    /// re-mounted landing continues the join once the session lands.
    public func armCallJoinAutoResume(guestName: String? = nil) {
        callJoinAutoResume = true
        callJoinGuestName = guestName
    }

    /// The name parked by ``armCallJoinAutoResume(guestName:)``, once.
    func takeCallJoinGuestName() -> String? {
        let name = callJoinGuestName
        callJoinGuestName = nil
        return name
    }

    /// ADR-0032 — read + clear the auto-resume flag (one-shot).
    public func consumeCallJoinAutoResume() -> Bool {
        let value = callJoinAutoResume
        callJoinAutoResume = false
        return value
    }
}
