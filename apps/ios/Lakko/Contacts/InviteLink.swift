// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// Sealed invite links (ADR-0028 and ADR-0009, amended 2026-09-28): the rules
/// every kind (`/i/`, `/c/`, `/g/`) shares, kept out of SwiftUI so a unit test
/// can drive them.
///
/// A link reads `https://<host>/<k>/<8-char id>#<16-char secret>` (pointer) or
/// `https://<host>/<k>/<base64url sealed blob>#<16-char secret>` (the inline
/// fallback, used only when the upload fails on transport). The capability is
/// encrypted under a key derived from the 12-byte secret, so the fragment is
/// not decoration: a link without it, or with anything but a well-formed
/// secret, opens nothing and is **damaged**. The secret is never sent to a
/// server and never logged.
public enum InviteLink {
    /// Width of the link secret in bytes (12), read from the core.
    public static var secretLen: Int { Int(inviteLinkSecretLen()) }

    /// Width of the link secret in the URL (16 base64url characters).
    public static var secretUrlLen: Int { Int(inviteLinkSecretUrlLen()) }

    /// What a link's fragment amounts to.
    public enum Fragment: Equatable {
        /// No fragment (a chat app that strips fragments, a hand-typed URL).
        case absent
        /// A well-formed 12-byte link secret.
        case secret(Data)
        /// Anything else: wrong length, wrong alphabet, padding.
        case malformed
    }

    /// Read a fragment: absent, a 12-byte secret, or malformed. Only exactly
    /// 16 base64url characters (no padding) decoding to 12 bytes is a secret.
    public static func readFragment(_ fragment: String?) -> Fragment {
        guard let fragment, !fragment.isEmpty else { return .absent }
        guard fragment.count == secretUrlLen,
              isBase64Url(fragment),
              let bytes = base64UrlDecodeNoPad(fragment),
              bytes.count == secretLen else { return .malformed }
        return .secret(bytes)
    }

    /// The link secret a fragment carries, or `nil` when the link is damaged
    /// (no fragment, or one that is not a secret). Every kind refuses a `nil`
    /// before anything is fetched.
    public static func secret(fromFragment fragment: String?) -> Data? {
        if case .secret(let bytes) = readFragment(fragment) { return bytes }
        return nil
    }

    /// Split a link into the part before `#` and the fragment after it (`nil`
    /// when there is none). Slug parsers must only ever see `base`.
    public static func split(_ link: String) -> (base: String, fragment: String?) {
        guard let hash = link.firstIndex(of: "#") else { return (link, nil) }
        let fragment = String(link[link.index(after: hash)...])
        return (String(link[..<hash]), fragment.isEmpty ? nil : fragment)
    }

    /// Append the secret to a link as its fragment (base64url, no padding).
    /// Any fragment the URL already carried is replaced, never doubled.
    public static func withSecret(_ url: String, secret: Data) -> String {
        split(url).base + "#" + base64UrlEncodeNoPad(secret)
    }

    /// The pointer link: `<origin><prefix><8-char id>#<secret>`.
    public static func pointer(
        origin: String, prefix: String, blobId: Data, secret: Data
    ) -> String {
        withSecret(origin + prefix + base64UrlEncodeNoPad(blobId), secret: secret)
    }

    /// The inline fallback: `<origin><prefix><base64url sealed>#<secret>`.
    public static func inline(
        origin: String, prefix: String, sealed: Data, secret: Data
    ) -> String {
        withSecret(origin + prefix + base64UrlEncodeNoPad(sealed), secret: secret)
    }

    /// Whether a failed upload may fall back to the inline link: only when the
    /// request never got an answer (DNS, TLS, no route, a censored network).
    /// Any HTTP status is the server's answer and surfaces.
    public static func uploadFallsBackInline(_ error: Error) -> Bool {
        if let discovery = error as? DiscoveryClientError, case .network = discovery {
            return true
        }
        return false
    }
}

/// The copy of a `/c/` link that is damaged: no fragment, a fragment that is
/// not a link secret, or a secret the blob does not open under. The same
/// sentence as every other kind (docs/ux/group-invite-page.md section 3), so
/// it reuses the one constant rather than a second wording.
public let callInviteDamagedMessage = groupInviteDamagedMessage
