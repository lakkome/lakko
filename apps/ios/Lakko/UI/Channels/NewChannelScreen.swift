// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore
import SwiftUI

/// The create form (UX spec section 5.4): a name, an optional description,
/// an optional `@handle` others can find the channel by.
struct NewChannelScreen: View {
    @ObservedObject var state: ChannelsUiState
    let client: ChannelsClient
    /// ADR-0017: the canonical AccountIdentity (phrase) seed from the active
    /// session. Used to build a transient `Identity` inside `create` — the
    /// MLS PublisherGroup founding member.
    let accountIdentitySeed: Data
    /// G4-5 — bearer token provider for the session-token-gated `@handle`
    /// claim (`/discovery/handle/claim`, ADR-0087). `nil` → cannot claim a
    /// handle (an empty handle skips discovery entirely, so create still
    /// works).
    var sessionTokenProvider: () -> String? = { nil }
    /// ADR-0028 OQ3 — open the co-publisher join flow.
    ///
    /// It lives here because `docs/ux/navigation.md` section 3 fixes the
    /// create menu at exactly three items and Channels is no longer a
    /// destination with an add menu of its own. "I was sent a link to publish
    /// to someone else's channel" is the same intent as "I want a channel", so
    /// the one screen that already answers that intent is where it goes.
    /// `nil` hides it.
    var onJoinAsCoPublisher: (() -> Void)?
    @Environment(\.dismiss) private var dismiss

    @State private var name: String = ""
    @State private var description: String = ""
    /// Optional `@handle` claimed via OPRF discovery (G4-5) so others can
    /// subscribe by handle instead of the raw channel id.
    @State private var handle: String = ""
    @State private var busy = false
    @State private var errorMessage: UiText?
    /// The live taken check of the handle (who-can-find-me 6.1): 500 ms
    /// after typing stops, an answer reused for a minute.
    @State private var handleTaken = false
    @State private var handleRateLimited = false
    @State private var takenCheck: Task<Void, Never>?
    /// ADR-0061 — every line follows the interface language.
    @ObservedObject private var l10n = Localization.shared

    var body: some View {
        Form {
            Section {
                TextField(l10n.t("channels.field.name"), text: $name)
                TextField(l10n.t("channels.field.description_optional"), text: $description, axis: .vertical)
                    .lineLimit(2...4)
            } footer: {
                Text(l10n.t("channels.new.subtitle"))
            }
            Section {
                TextField(l10n.t("channels.new.handle_label"), text: $handle)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled(true)
            } footer: {
                if handleTaken {
                    Text(l10n.t("channels.new.error.handle_taken", texts: ["handle": "@\(bareHandle)"]))
                        .foregroundStyle(Color.lakkoDanger)
                } else if handleRateLimited {
                    Text(l10n.t("common.error.too_many_attempts"))
                } else {
                    Text(l10n.t("channels.new.handle_hint"))
                }
            }
            if let errorMessage {
                Section {
                    Text(errorMessage.text)
                        .foregroundStyle(Color.lakkoDanger)
                }
            }
            if let onJoinAsCoPublisher {
                Section {
                    Button(l10n.t("channels.join.title"), action: onJoinAsCoPublisher)
                        .disabled(busy)
                } footer: {
                    Text(l10n.t("channels.join.subtitle"))
                }
            }
        }
        .onChange(of: handle) { _, _ in scheduleTakenCheck() }
        .scrollContentBackground(.hidden)
        .background(Color.lakkoBackground)
        .navigationTitle(l10n.t("channels.new.title"))
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .topBarTrailing) {
                Button(l10n.t(busy ? "chats.new_group.creating" : "channels.new.create")) {
                    Task { await create() }
                }
                .disabled(busy || name.trimmingCharacters(in: .whitespaces).isEmpty)
            }
        }
    }

    private var bareHandle: String {
        String(handle.trimmingCharacters(in: .whitespaces).drop(while: { $0 == "@" }))
    }

    private func scheduleTakenCheck() {
        takenCheck?.cancel()
        handleTaken = false
        handleRateLimited = false
        let typed = ConversationHandles.canonical(bareHandle)
        guard typed.count >= 3, ConversationHandles.wellFormed(typed) else { return }
        if let held = HandleTakenCheck.held(typed) {
            handleTaken = held == .taken
            return
        }
        takenCheck = Task { @MainActor in
            try? await Task.sleep(nanoseconds: HandleTakenCheck.debounceNanoseconds)
            guard !Task.isCancelled, typed == ConversationHandles.canonical(bareHandle) else { return }
            let answer = await HandleTakenCheck.check(typed)
            guard !Task.isCancelled, typed == ConversationHandles.canonical(bareHandle) else { return }
            handleTaken = answer == .taken
            handleRateLimited = answer == .rateLimited
        }
    }

    private func create() async {
        if busy { return }
        // A handle the check found taken in the last minute: no claim.
        if !bareHandle.isEmpty, HandleTakenCheck.held(bareHandle) == .taken {
            handleTaken = true
            errorMessage = .key("channels.new.error.handle_taken", texts: ["handle": "@\(bareHandle)"])
            return
        }
        busy = true
        defer { busy = false }
        errorMessage = nil
        // The account this channel is made for, before any wait: a founding
        // mark is filed under it and under no account signed in later.
        let accountIdB64 = AcceptedContactsStore.boundAccountIdB64
        let wantedHandle = handle.trimmingCharacters(in: .whitespaces)
        let cleanHandle = String(wantedHandle.drop(while: { $0 == "@" }))
        // A create that failed after claiming this handle: the same channel
        // id, which the claim answers as already this channel's.
        let id = accountIdB64.flatMap {
            PendingChannelHandleStore.channelId(forHandle: cleanHandle, accountIdB64: $0)
        } ?? ChannelsComposer.freshChannelIdHex()
        do {
            // G4-5: claim the optional `@handle` FIRST (before the channel
            // exists on the channels service) so a first-come-first-served
            // collision (409) aborts cleanly without leaving a half-created
            // channel. An empty handle skips discovery entirely — the raw
            // channel id still works. The mapping is `@handle -> channel_id`;
            // the OPRF hash is all the server ever sees.
            if !wantedHandle.isEmpty {
                // The tokens the claim spends are drawn with the session.
                guard sessionTokenProvider() != nil else {
                    errorMessage = .key("channels.new.error.sign_in")
                    return
                }
                // Kept before the claim goes out: an answer that does not
                // arrive may still have claimed the name.
                if let accountIdB64 {
                    PendingChannelHandleStore.keep(
                        handle: cleanHandle, channelIdHex: id, accountIdB64: accountIdB64)
                }
                do {
                    _ = try await ChannelDiscovery.registerChannelHandle(
                        handle: wantedHandle,
                        channelIdHex: id,
                        ownerIdentitySeed: accountIdentitySeed,
                        name: name.trimmingCharacters(in: .whitespaces),
                        description: description.trimmingCharacters(in: .whitespacesAndNewlines)
                    )
                } catch let refusal as DiscoveryClient.HandleWriteError where refusal.code == "name-taken" {
                    HandleTakenCheck.note(cleanHandle, .taken)
                    if let accountIdB64 {
                        PendingChannelHandleStore.forget(handle: cleanHandle, accountIdB64: accountIdB64)
                    }
                    errorMessage = .key("channels.new.error.handle_taken", texts: ["handle": "@\(cleanHandle)"])
                    return
                } catch let refusal as DiscoveryClient.HandleWriteError where refusal.status == 429 {
                    // Discovery's rate limit: its own line, not "could not claim".
                    errorMessage = .key("common.error.too_many_attempts")
                    return
                } catch let token as AnonTokenUnavailable {
                    // ADR-0054 S3: the allowance copy, or the network copy.
                    NSLog("Channels: handle registration token: %@", String(describing: token))
                    errorMessage = UiText { token.errorDescription ?? AnonTokenCopy.networkFailure }
                    return
                } catch {
                    NSLog("Channels: handle registration failed: %@", String(describing: error))
                    if case .network? = error as? DiscoveryClientError {
                        errorMessage = .key("channels.new.error.handle_offline")
                    } else {
                        errorMessage = .key("channels.new.error.handle")
                    }
                    return
                }
            }

            // ADR-0059 section 1 — the channel is registered under the owner
            // key the CURRENT seed's identity derives for it (`owner`). The
            // PublisherGroup is founded in the durable channel provider
            // (``ChannelPublisherIdentity``, G-50), so the content seed comes
            // from a real MLS exporter and survives a relaunch.
            let owner = try Identity.fromSeed(seed: accountIdentitySeed)
            _ = try await client.createChannel(channelIdHex: id, accountIdentity: owner)
            let identity = try ChannelPublisherIdentity.shared.identity(currentSeed: accountIdentitySeed)
            let keys = try ChannelsComposer.freshPublisherKeys(
                identity: identity,
                accountIdB64: accountIdB64,
                channelIdHex: id,
                // G-29 — reuse the persisted per-channel signing seed if this
                // device has published to `id` before (nil for a fresh id).
                existingSigningSeed: state.persistedPublisherSigningSeed(channelId: id)
            )
            // ADR-0057 — the publisher journal is claimed with this account's
            // root BEFORE the channel is announced; a channel whose journal
            // could not be claimed is not added.
            let publisherGroup = try CoPublisher.openPublisherGroup(
                identity: identity, accountIdB64: accountIdB64, channelIdHex: id)
            // The founded group is on disk before the channel is announced:
            // a channel whose group a relaunch would not find is not added.
            try ChannelPublisherIdentity.shared.persistOrThrow()
            let signer = try publisherGroup.dsAccessSigner(identity: identity)
            let groupId = try publisherGroup.groupId()
            do {
                try await JournalOwnership.claimOrReset(
                    root: try CoPublisher.rootSigner(identity: identity, publisherGroup: publisherGroup),
                    journalId: groupId, epoch: signer.epoch(), epochKey: signer.verifyingKey(),
                    ds: DsClient())
                JournalFoundingMarks.shared.clear(journalIdHex: DsClient.pseudonymHex(groupId))
            } catch {
                NSLog("Channels: publisher journal claim failed: %@", String(describing: error))
                errorMessage = .key("channels.new.error.create")
                return
            }
            state.addChannel(.init(
                id: id,
                localName: name.trimmingCharacters(in: .whitespaces),
                localDescription: description.trimmingCharacters(in: .whitespaces),
                addedAtMs: UInt64(Date().timeIntervalSince1970 * 1000),
                handle: cleanHandle
            ))
            state.setKeys(channelId: id, keys: keys)
            if let accountIdB64 {
                PendingChannelHandleStore.forget(handle: cleanHandle, accountIdB64: accountIdB64)
            }
            // ADR-0073 phase 2 (UX 16.6) — which key generation registered the
            // owner key, so a later reset on this device can say the channel
            // is orphaned.
            if let generation = state.currentKeyGeneration {
                state.recordOwnerKeyGeneration(channelId: id, generation: generation)
            }
            // ADR-0051 §4 — the metadata record, right after creation, so a
            // subscriber's first read titles the channel. The channel exists
            // either way; a failed post leaves the local name in place and the
            // Manage screen's Save posts a new record.
            if let meta = ChannelMetaRecord.fresh(
                name: name, description: description,
                nowMs: UInt64(Date().timeIntervalSince1970 * 1000)) {
                do {
                    try await ChannelMetaRecord.post(
                        state: state, client: client, channelId: id, meta: meta,
                        accountIdentity: owner)
                } catch {
                    NSLog("Channels: metadata record post failed: %@", String(describing: error))
                }
            }
            dismiss()
        } catch let err as LegacyChannelError {
            errorMessage = UiText { err.message }
        } catch {
            NSLog("Channels: create failed: %@", String(describing: error))
            errorMessage = .key("channels.new.error.create")
        }
    }
}
