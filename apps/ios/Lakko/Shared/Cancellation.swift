// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// One answer to "did this request fail, or was it called off?".
///
/// A task is cancelled when the person leaves the screen that started it (a
/// `.task` or `.refreshable` body, a stored `Task` that is cancelled or
/// replaced). That is not a failed request: nothing is reported for it, no
/// retry is scheduled for it, and cleanup meant for a real failure does not
/// run for it. Busy flags and locks are still released, which is what `defer`
/// is for.
///
/// What a cancellation looks like when it reaches a `catch`:
///
///   - `CancellationError`, from `Task.sleep`, `Task.checkCancellation` and
///     the code in this app that throws it itself;
///   - `URLError(.cancelled)`, from `URLSession`;
///   - either of those inside one of the app's own error wrappers (every
///     REST client files a transport error under `.network`).
///
/// What it never looks like: the anti-censorship tunnel and the UniFFI core
/// are not cancellable. The tunnelled request runs in a detached task that is
/// awaited to its end, and a core call is synchronous, so neither throws
/// anything on a cancellation.
enum Cancellation {
    /// `true` when `error` is a cancellation, unwrapped through the app's own
    /// error wrappers.
    static func isCancellation(_ error: Error) -> Bool {
        isCancellation(error, depth: 0)
    }

    /// `true` when the work that threw `error` was called off: the error is a
    /// cancellation, or the current task is cancelled. The second half covers
    /// the errors that keep no underlying cause (a token that could not be
    /// obtained, a result enum with a bare `failed`): on a cancelled task the
    /// person has left, whatever the error says.
    static func endedByCancellation(_ error: Error) -> Bool {
        Task.isCancelled || isCancellation(error)
    }

    /// Run `work` to its end whether or not the caller is cancelled, and wait
    /// for it. For work that belongs to the session rather than to the screen
    /// that started it: a flow of several writes that must not stop half way
    /// because the person swiped back.
    @MainActor
    static func shielded<T>(_ work: @escaping @MainActor () async -> T) async -> T {
        await Task { @MainActor in await work() }.value
    }

    /// How many wrappers deep an error is followed. The app's wrappers nest
    /// two or three deep at most; the bound keeps a hostile or cyclic error
    /// from recursing without end.
    private static let maxDepth = 8

    private static func isCancellation(_ error: Error, depth: Int) -> Bool {
        if error is CancellationError { return true }
        // Work that outlived the account it was started for
        // (``BindingEpoch``) was called off just the same.
        if error is StaleBindingError { return true }
        // ...and so was a durable write refused for an account whose session
        // ended (``AccountStoreGate``).
        if error is AccountStoreGate.Sealed { return true }
        if let url = error as? URLError { return url.code == .cancelled }
        guard depth < maxDepth, let inner = underlying(of: error) else { return false }
        return isCancellation(inner, depth: depth + 1)
    }

    /// The error one of the app's wrappers carries, or `nil` for anything
    /// else.
    private static func underlying(of error: Error) -> Error? {
        switch error {
        case let e as DsClientError:
            if case .network(_, let inner) = e { return inner }
        case let e as AuthClientError:
            if case .network(_, let inner) = e { return inner }
        case let e as DiscoveryClientError:
            if case .network(_, let inner) = e { return inner }
        case let e as StorageClientError:
            if case .network(_, let inner) = e { return inner }
        case let e as PushClientError:
            if case .network(_, let inner) = e { return inner }
        case let e as WebauthnClientError:
            if case .network(_, let inner) = e { return inner }
        case let e as TransparencyClientError:
            if case .network(_, let inner) = e { return inner }
        case let e as CallJwtClientError:
            if case .network(_, let inner) = e { return inner }
        case let e as ChatError:
            if case .wrapped(_, let inner) = e { return inner }
        case let e as ContactsError:
            if case .wrapped(let inner) = e { return inner }
        case let e as AttachmentBridgeError:
            switch e {
            case .upload(let inner), .download(let inner): return inner
            default: return nil
            }
        default:
            break
        }
        return nil
    }
}
