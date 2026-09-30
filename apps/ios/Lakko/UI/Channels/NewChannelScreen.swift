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
    /// registration (`/discovery/channel/register`). `nil` → cannot claim a
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
    /// ADR-0058 — every line follows the interface language.
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
                Text(l10n.t("channels.new.handle_hint"))
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

    private func create() async {
        if busy { return }
        busy = true
        defer { busy = false }
        errorMessage = nil
        let id = ChannelsComposer.freshChannelIdHex()
        let wantedHandle = handle.trimmingCharacters(in: .whitespaces)
        let cleanHandle = String(wantedHandle.drop(while: { $0 == "@" }))
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
                do {
                    _ = try await ChannelDiscovery.registerChannelHandle(
                        handle: wantedHandle,
                        channelIdHex: id
                    )
                } catch let DiscoveryClientError.http(status, _, _) where status == 409 {
                    errorMessage = .key("channels.new.error.handle_taken", texts: ["handle": "@\(cleanHandle)"])
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

            _ = try await client.createChannel(channelIdHex: id)
            // G-27: build a transient `Identity` from the canonical
            // AccountIdentity (phrase) seed (ADR-0017), hand it to the
            // composer so the channel content-key seed comes from a real MLS
            // PublisherGroup exporter. The identity is throwaway —
            // `freshPublisherKeys` builds a throwaway `Group` from it for the
            // duration of the call. `Identity` releases its handle via ARC at
            // scope-exit — UniFFI's Swift surface has no explicit `close()`.
            let identity = try Identity.fromSeed(seed: accountIdentitySeed)
            let keys = try ChannelsComposer.freshPublisherKeys(
                identity: identity,
                channelIdHex: id,
                // G-29 — reuse the persisted per-channel signing seed if this
                // device has published to `id` before (nil for a fresh id).
                existingSigningSeed: state.persistedPublisherSigningSeed(channelId: id)
            )
            state.addChannel(.init(
                id: id,
                localName: name.trimmingCharacters(in: .whitespaces),
                localDescription: description.trimmingCharacters(in: .whitespaces),
                addedAtMs: UInt64(Date().timeIntervalSince1970 * 1000),
                handle: cleanHandle
            ))
            state.setKeys(channelId: id, keys: keys)
            // ADR-0051 §4 — the metadata record, right after creation, so a
            // subscriber's first read titles the channel. The channel exists
            // either way; a failed post leaves the local name in place and the
            // Manage screen's Save posts a new record.
            if let meta = ChannelMetaRecord.fresh(
                name: name, description: description,
                nowMs: UInt64(Date().timeIntervalSince1970 * 1000)) {
                do {
                    try await ChannelMetaRecord.post(
                        state: state, client: client, channelId: id, meta: meta)
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
