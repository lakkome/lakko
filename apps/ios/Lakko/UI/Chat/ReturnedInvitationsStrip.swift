// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import SwiftUI

/// ADR-0101 section 7 — the channel invitations a return brought whose old
/// member card did not verify under this account's previous key: each drawn
/// as the ordinary invitation card (Subscribe, or Accept for a publisher
/// invitation), above the chat list. Never accepted silently and never
/// dropped; a card leaves once the account reads its channel
/// (``ChannelReturnRuntime/settleRecovered(channels:seed:accountIdB64:)``).
/// iOS mirror of `apps/web/src/channels/ReturnedInvitations.tsx`.
struct ReturnedInvitationsStrip: View {
    @ObservedObject var channels: ChannelsUiState
    let accountIdB64: String
    @ObservedObject private var store = ReturnedInvitationStore.shared

    private var shown: [(channelIdHex: String, card: Data)] {
        _ = store.version
        let reading = Set(channels.channels.map(\.id))
        return ReturnedInvitationStore.toShow(held: store.read(accountIdB64: accountIdB64), reading: reading)
            .compactMap { entry in base64Decode(entry.cardB64).map { (entry.channelIdHex, $0) } }
    }

    var body: some View {
        let items = shown
        if !items.isEmpty {
            VStack(alignment: .leading, spacing: 8) {
                ForEach(items, id: \.channelIdHex) { item in
                    if let facts = ChannelInviteCardFacts.facts(card: item.card) {
                        ChannelInvitationCard(facts: facts, outgoing: false)
                    } else if let facts = CoPublisherCardFacts.facts(card: item.card) {
                        CoPublisherInvitationCard(facts: facts, outgoing: false)
                    }
                }
            }
            .padding(.horizontal, 16)
            .padding(.vertical, 8)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(Color.lakkoSurface)
        }
    }
}
