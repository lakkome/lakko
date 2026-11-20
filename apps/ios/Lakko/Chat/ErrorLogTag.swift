// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// A log tag for an error that carries no identifier (ADR-0091 phase 1b).
/// Mirror of web `chat/log-tag.ts`.
///
/// An error's description is not safe to log: a DS error names its endpoint,
/// and the endpoint names the journal (`/ds/group/<hex>/...`). The tag keeps
/// what explains a failure and nothing that names a conversation, a row, a
/// key or an account: the error's type along the wrapping chain, the HTTP
/// status of a DS refusal, and a typed membership refusal code. Never the
/// message, the body or the endpoint.
enum ErrorLogTag {

    /// At most this many wrapping levels are followed.
    private static let maxDepth = 6

    /// `Type[>Type...][ status=N][ refusal=code]`.
    static func of(_ error: Error) -> String {
        var names: [String] = []
        var status: Int?
        var current: Error? = error
        var depth = 0
        while let e = current, depth < maxDepth {
            depth += 1
            if let ds = e as? DsClientError {
                switch ds {
                case .argument: names.append("DsClientError.argument")
                case .http(let s, _, _):
                    names.append("DsClientError.http")
                    if status == nil { status = s }
                case .network: names.append("DsClientError.network")
                case .clockSkew: names.append("DsClientError.clockSkew")
                }
                current = nil
            } else if let chat = e as? ChatError, case .wrapped(_, let underlying) = chat {
                names.append("ChatError.wrapped")
                current = underlying
            } else {
                names.append(String(describing: type(of: e)))
                current = nil
            }
        }
        let refusal = membershipRefusalReason(of: error).map { FailedApplyKind.code($0) }
        return names.joined(separator: ">")
            + (status.map { " status=\($0)" } ?? "")
            + (refusal.map { " refusal=\($0)" } ?? "")
    }
}
