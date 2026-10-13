// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// "Is the account this work was started for still the one that is bound?"
///
/// The repositories and the sync runtime live for the whole process: they are
/// not rebuilt on sign-out, they are told the session ended and drop their
/// state. Nothing cancels an unstructured `Task`, and a cancelled one still
/// runs the code after an `await` that returned, so work started for account
/// A (a poll, an invite accept, a sign-in round trip) goes on after A signed
/// out, and what follows its `await` writes into whatever the owner holds by
/// then: nothing, or account B's state.
///
/// The owner advances its epoch whenever its binding ends or changes; work
/// captures the epoch when it starts and asks after every `await`, before any
/// write or act. Mirror of `apps/web/src/auth/binding-epoch.ts`.
///
/// A counter and not the account id alone: signing out and back in to the
/// SAME account is a new binding too, with new handles, and work from the
/// first must not complete into the second.
@MainActor
final class BindingEpoch {
    private var epoch: UInt64 = 0

    init() {}

    /// The binding ended or changed (sign-out, forget device, another account
    /// landed, the owner was cleared). Every ``Captured`` taken before this
    /// call answers `false` from now on.
    func advance() {
        epoch &+= 1
    }

    /// Remember the binding that is current now.
    func capture() -> Captured {
        Captured(owner: self, epoch: epoch)
    }

    /// One binding, as work that started under it holds it.
    @MainActor
    struct Captured {
        fileprivate let owner: BindingEpoch
        fileprivate let epoch: UInt64

        /// `true` while the binding this was captured under is still the
        /// owner's.
        var isCurrent: Bool { owner.epoch == epoch }

        /// Throw ``StaleBindingError`` once the binding has moved. For work
        /// whose caller must not read the outcome as success.
        func require() throws {
            if !isCurrent { throw StaleBindingError() }
        }
    }
}

/// Thrown by work that found its binding gone after an `await`. Not a failed
/// request: ``Cancellation/isCancellation(_:)`` answers `true` for it, so no
/// screen reports it and no retry is scheduled for it.
struct StaleBindingError: Error, Equatable {}
