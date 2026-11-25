// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore
import SwiftUI
import UIKit

/// Links in a message's text (`docs/ux/message-links.md`). Which runs are links
/// is the core's rule (`splitMessageLinks`); what a tap does is decided here,
/// from the address alone. The twin of `apps/web/src/chat/message-text.ts`.
enum MessageLinks {

    /// Where a tap on a link goes.
    enum Open: Equatable {
        /// A Lakko link of this deployment: the app's own link intake, so the
        /// screen is the one a link from outside reaches.
        case inApp
        /// Anything else: the system's default browser.
        case browser
    }

    private static let token: Set<Character> = Set(
        "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789_-")
    private static let linkIdLength = 8
    private static let secretLength = 16

    static func open(_ url: URL, ownOrigin: String) -> Open {
        isOwnLink(url.absoluteString, ownOrigin: ownOrigin) ? .inApp : .browser
    }

    /// Whether `link` is `/i/`, `/c/` or `/g/` on `ownOrigin` with its ledger id
    /// and its secret, and nothing else. Same scheme, host and port; no query.
    /// A link that only looks like one of ours goes to the browser.
    ///
    /// Taken apart by hand rather than by a URL parser, as
    /// ``CallInviteLink/callInviteOf(_:ownOrigin:)`` does: a parser normalises,
    /// and the rule is about the characters sent.
    static func isOwnLink(_ link: String, ownOrigin: String) -> Bool {
        guard let own = CallInviteLink.origin(of: ownOrigin),
              let schemeEnd = link.range(of: "://") else { return false }
        let afterScheme = link[schemeEnd.upperBound...]
        guard let pathStart = afterScheme.firstIndex(of: "/") else { return false }
        let linkOrigin = String(link[..<pathStart]).lowercased()
        guard linkOrigin == own else { return false }

        let rest = afterScheme[pathStart...]
        guard let hash = rest.firstIndex(of: "#") else { return false }
        let parts = rest[..<hash].split(separator: "/", omittingEmptySubsequences: false)
        guard parts.count == 3, parts[0].isEmpty, ["i", "c", "g"].contains(parts[1]) else { return false }
        return isToken(parts[2], length: linkIdLength)
            && isToken(rest[rest.index(after: hash)...], length: secretLength)
    }

    private static func isToken(_ value: Substring, length: Int) -> Bool {
        value.count == length && value.allSatisfy { token.contains($0) }
    }

    /// `text` with its links marked: underlined, so the colour is not the only
    /// signal. Everything else stays as it was sent, and no character is
    /// dropped. A run the system cannot read as an address stays plain.
    static func attributed(_ text: String) -> AttributedString {
        let key = text as NSString
        if let held = attributedCache.object(forKey: key) { return held.value }
        var out = AttributedString()
        for piece in splitMessageLinks(text: text) {
            var run = AttributedString(piece.text)
            if piece.isLink, let url = URL(string: piece.text) {
                run.link = url
                run.swiftUI.underlineStyle = .single
            }
            out.append(run)
        }
        attributedCache.setObject(Held(value: out), forKey: key)
        return out
    }

    /// A row's body runs far more often than its text changes, and each run
    /// would otherwise cross into the core.
    private static let attributedCache: NSCache<NSString, Held> = {
        let cache = NSCache<NSString, Held>()
        cache.countLimit = 512
        return cache
    }()

    private final class Held {
        let value: AttributedString
        init(value: AttributedString) { self.value = value }
    }
}

/// A message's text with its links live. Font and text colour come from the
/// caller, as they would for a plain `Text`, so the bubble's wrapping, the
/// edited mark and the time beside it stay as they are.
///
/// The links are the `Text`'s own runs rather than tap gestures: nothing here
/// takes the press from the bubble, so the message menu still opens over a
/// link. `linkColor` is a tint because SwiftUI draws a link run in the tint
/// and ignores the text colour.
struct MessageText: View {
    let text: String
    let linkColor: Color
    /// Called with a Lakko link of this deployment.
    let onOwnLink: (URL) -> Void

    var body: some View {
        Text(MessageLinks.attributed(text))
            .tint(linkColor)
            .environment(\.openURL, OpenURLAction { url in
                switch MessageLinks.open(url, ownOrigin: ApiEndpoints.webOrigin) {
                case .inApp:
                    // The composer gives up the keyboard before the link's
                    // screen is pushed. Left as first responder, UIKit put it
                    // back on the way back and the keyboard rose over the
                    // composer without the chat moving up for it (seen
                    // 2026-11-25 after Back from an `/i/` link). Clearing the
                    // focus binding on disappear came too late for that.
                    UIApplication.shared.sendAction(
                        #selector(UIResponder.resignFirstResponder), to: nil, from: nil, for: nil)
                    onOwnLink(url)
                    return .handled
                case .browser:
                    return .systemAction(url)
                }
            })
    }
}
