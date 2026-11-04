// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// A Lakko HTTP service a native client talks to.
///
/// The raw value is the path segment the service lives under on the native
/// API host. The server exposes the same prefix-less shape in two places:
///
///   * production: `https://api.lakko.me/<segment>/...`
///   * test stack: `https://test.lakko.me/native/<segment>/...`
///
/// so one API base URL per build selects the whole deployment.
public enum ApiService: String, CaseIterable, Sendable {
    case auth
    case ds
    case storage
    case discovery
    case pushGw = "push-gw"
    case channels
    case gifs

    /// The base URL used when the build carries no API base URL: the service's
    /// local development bind, which the iOS simulator reaches on the Mac's
    /// loopback. These are the values every client hardcoded before the API
    /// base URL existed, so local development is unchanged.
    public var localDevBaseURL: String {
        switch self {
        case .auth: return "http://localhost:8081"
        case .ds: return "http://localhost:8082"
        case .storage: return "http://localhost:8083"
        case .discovery: return "http://localhost:8084"
        case .pushGw: return "http://localhost:8085"
        case .channels: return "http://localhost:8086"
        case .gifs: return "http://localhost:8091"
        }
    }
}

/// The one build-time API base URL every native REST client derives its
/// service base URL from.
///
/// Read from the Info.plist key `LakkoApiBaseUrl`, which `project.yml` fills
/// from the `LAKKO_API_BASE_URL` build setting per build configuration. Both
/// the app and the NotificationServiceExtension carry the key, so the NSE's
/// DS fetch follows the same deployment as the app.
///
/// When the key is empty (or absent) every service falls back to its local
/// development bind (``ApiService/localDevBaseURL``). A release build without
/// a base URL logs that once, so a misconfigured release is visible in the
/// device log instead of silently dialling `localhost`.
///
/// FFI-clean (Foundation only): compiled into the NSE target as a sibling
/// source, like the other `Shared/` stores.
public enum ApiEndpoints {

    /// The Info.plist key a build sets.
    public static let infoPlistKey = "LakkoApiBaseUrl"

    /// The configured API base URL, normalised, or `nil` when unset.
    /// Info.plist is immutable at runtime, so it is read once.
    public static let configuredApiBaseURL: String? = normalizedBase(
        Bundle.main.object(forInfoDictionaryKey: infoPlistKey) as? String
    )

    /// The base URL (no trailing slash) for `service` in this build.
    /// Info.plist key carrying the web origin this build mints its links under.
    public static let webOriginInfoPlistKey = "LakkoWebOrigin"

    /// The production web origin, the fallback when a build sets none.
    public static let productionWebOrigin = "https://lakko.me"

    /// The web origin this build shares its links under (`/i/`, `/g/`, `/c/`).
    /// It must be the host the same build's servers live behind: a link minted
    /// here is opened by people on that stack's web client and by phones whose
    /// app claims that host, so a Debug build against the test stack mints
    /// `https://test.lakko.me/...`, never the production host.
    public static let webOrigin: String = normalizedBase(
        Bundle.main.object(forInfoDictionaryKey: webOriginInfoPlistKey) as? String
    ) ?? productionWebOrigin

    public static func baseURL(for service: ApiService) -> String {
        if configuredApiBaseURL == nil {
            _ = warnIfReleaseWithoutBase
        }
        return resolve(apiBase: configuredApiBaseURL, service: service)
    }

    /// Pure resolution: `<apiBase>/<segment>` when `apiBase` is non-empty after
    /// trimming whitespace and trailing slashes, else the service's local
    /// development base URL.
    public static func resolve(apiBase: String?, service: ApiService) -> String {
        guard let base = normalizedBase(apiBase) else {
            return service.localDevBaseURL
        }
        return base + "/" + service.rawValue
    }

    /// Trim surrounding whitespace and every trailing `/`; `nil` when nothing
    /// is left.
    public static func normalizedBase(_ raw: String?) -> String? {
        guard let raw else { return nil }
        var trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        while trimmed.hasSuffix("/") {
            trimmed.removeLast()
        }
        return trimmed.isEmpty ? nil : trimmed
    }

    /// Evaluated at most once per process (a lazily initialised global).
    private static let warnIfReleaseWithoutBase: Void = {
        #if !DEBUG
        NSLog(
            "[api] WARN LakkoApiBaseUrl is empty in a release build; "
                + "every service falls back to its localhost development address"
        )
        #endif
    }()
}
