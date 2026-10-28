// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// The TURN credential a call's ICE configuration adds (G3-1-turn), from the
/// anonymous call-admission exchange (ADR-0078 section 2). Mirror of the web
/// `turnProviderForCall` / the Android `TurnCredentialClient.kt`.
///
/// There is no bearer route for TURN any more: a credential is minted beside
/// an SFU call token by `POST /call/jwt`, which spends one anonymous
/// `call-join` token, and its username tag is random per credential, so the
/// relay (and its logs) never see an account. An SFU call already holds one
/// (``init(preMinted:)``); a one-to-one call, which needs no SFU token, mints
/// one for its call id the same way (``forCall(callId:jwtClient:)``).
///
/// **Fail-soft by contract.** No credential - a deployment without a relay, a
/// network failure, no token to be had - resolves to `nil`, and the call
/// proceeds on host candidates alone (``IceServers``: Lakko uses no public
/// STUN server). An availability choice about ICE candidates, never an E2EE
/// fallback (invariant #4 is untouched).
public final class TurnCredentialClient: @unchecked Sendable {
    private let mint: () async -> TurnCredential?
    private let lock = NSLock()
    private var minted: Task<TurnCredential?, Never>?

    /// A credential known up front (minted beside an SFU call token).
    public init(preMinted: TurnCredential?) {
        self.mint = { preMinted }
    }

    private init(mint: @escaping () async -> TurnCredential?) {
        self.mint = mint
    }

    /// A credential minted once, on first use, for `callId` through the
    /// anonymous call-join exchange.
    public static func forCall(
        callId: Data,
        jwtClient: CallJwtClient = CallJwtClient()
    ) -> TurnCredentialClient {
        TurnCredentialClient(mint: {
            do {
                return try await jwtClient.mint(callId: callId, role: .participant).turn
            } catch {
                NSLog("[lakko.turn] mint failed, proceeding on host candidates only")
                return nil
            }
        })
    }

    /// The TURN row to use, or `nil` when none could be minted. Minted at
    /// most once. ``IceServers/entries(for:)`` turns it into the call's list.
    func fetchEntryOrNil() async -> IceServers.Entry? {
        let task: Task<TurnCredential?, Never> = lock.withLock {
            if let minted { return minted }
            let t = Task { await self.mint() }
            minted = t
            return t
        }
        guard let credential = await task.value else { return nil }
        return IceServers.Entry(turn: credential)
    }
}
