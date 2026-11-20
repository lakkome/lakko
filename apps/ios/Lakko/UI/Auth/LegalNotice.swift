// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import SwiftUI

/// `docs/ux/legal-pages.md`: the Terms of Service and the Privacy Policy are
/// fixed paths on this deployment's web origin, opened in the system browser
/// (no in-app web view). Neither path is claimed by the app's universal links,
/// so the system hands them to the browser.
enum LegalPage: CaseIterable {
    case terms
    case privacy

    /// The page's path on the web origin.
    var path: String {
        switch self {
        case .terms: return "/terms"
        case .privacy: return "/privacy"
        }
    }

    /// The link word inside the sign-up notice.
    var linkKey: String {
        switch self {
        case .terms: return "legal.terms"
        case .privacy: return "legal.privacy"
        }
    }

    /// The Settings > About row title.
    var titleKey: String {
        switch self {
        case .terms: return "legal.terms_title"
        case .privacy: return "legal.privacy_title"
        }
    }

    /// The `{arg}` this page fills in `legal.signup_notice`.
    var noticeArg: String {
        switch self {
        case .terms: return "terms"
        case .privacy: return "privacy"
        }
    }

    /// The page's URL on `origin` (the production origin when `origin` is
    /// empty); `nil` only when the configured origin is not a URL at all.
    func url(origin: String = ApiEndpoints.webOrigin) -> URL? {
        let base = ApiEndpoints.normalizedBase(origin) ?? ApiEndpoints.productionWebOrigin
        guard let url = URL(string: base + path), url.scheme != nil, url.host != nil else {
            return nil
        }
        return url
    }
}

enum LegalNotice {
    /// The sign-up notice with its two link words as link runs.
    ///
    /// `format` renders `legal.signup_notice` with the given `{terms}` /
    /// `{privacy}` texts. The words go in behind private-use markers, so they
    /// are found again however a language orders them; the bidi isolates the
    /// formatter puts around each argument stay in the plain text beside it.
    /// A marker the message does not contain leaves its word out of the line;
    /// the view still offers both pages as accessibility actions.
    static func attributed(
        format: ([String: String]) -> String,
        word: (LegalPage) -> String,
        origin: String = ApiEndpoints.webOrigin
    ) -> AttributedString {
        var args: [String: String] = [:]
        for page in LegalPage.allCases {
            args[page.noticeArg] = marker(page)
        }
        var rest = Substring(format(args))
        var out = AttributedString()
        while true {
            let hits = LegalPage.allCases.compactMap { page -> (LegalPage, Range<Substring.Index>)? in
                guard let range = rest.range(of: marker(page)) else { return nil }
                return (page, range)
            }
            guard let (page, range) = hits.min(by: { $0.1.lowerBound < $1.1.lowerBound }) else {
                break
            }
            out.append(AttributedString(String(rest[..<range.lowerBound])))
            var link = AttributedString(word(page))
            if let url = page.url(origin: origin) {
                link.link = url
            }
            out.append(link)
            rest = rest[range.upperBound...]
        }
        out.append(AttributedString(String(rest)))
        return out
    }

    private static func marker(_ page: LegalPage) -> String {
        "\u{E000}\(page.noticeArg)\u{E001}"
    }
}

/// The muted line under a Create account button (`docs/ux/legal-pages.md`
/// section 2). The two link words are link runs of one `Text`, which
/// VoiceOver lists under its Links rotor; each page is also a named action.
struct LegalSignupNotice: View {
    @ObservedObject private var l10n = Localization.shared
    @Environment(\.openURL) private var openURL

    var body: some View {
        Text(LegalNotice.attributed(
            format: { l10n.t("legal.signup_notice", texts: $0) },
            word: { l10n.t($0.linkKey) }
        ))
        .font(.footnote)
        .foregroundStyle(Color.lakkoTextMuted)
        .tint(Color.lakkoAccent)
        .fixedSize(horizontal: false, vertical: true)
        .frame(maxWidth: .infinity, alignment: .leading)
        .environment(\.openURL, OpenURLAction { .systemAction($0) })
        .accessibilityAction(named: Text(l10n.t(LegalPage.terms.linkKey))) {
            if let url = LegalPage.terms.url() { openURL(url) }
        }
        .accessibilityAction(named: Text(l10n.t(LegalPage.privacy.linkKey))) {
            if let url = LegalPage.privacy.url() { openURL(url) }
        }
    }
}
