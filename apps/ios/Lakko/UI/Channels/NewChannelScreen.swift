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
    @Environment(\.dismiss) private var dismiss

    @State private var name: String = ""
    @State private var description: String = ""
    /// Optional `@handle` claimed via OPRF discovery (G4-5) so others can
    /// subscribe by handle instead of the raw channel id.
    @State private var handle: String = ""
    @State private var busy = false
    @State private var errorMessage: String?

    var body: some View {
        Form {
            Section {
                TextField("Name", text: $name)
                TextField("Description (optional)", text: $description, axis: .vertical)
                    .lineLimit(2...4)
            } footer: {
                Text("The name and description stay on this device.")
            }
            Section {
                TextField("@handle (optional)", text: $handle)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled(true)
            } footer: {
                Text("Others find the channel by its handle. Leave it blank to skip.")
            }
            if let errorMessage {
                Section {
                    Text(errorMessage)
                        .foregroundStyle(Color.lakkoDanger)
                }
            }
        }
        .scrollContentBackground(.hidden)
        .background(Color.lakkoBackground)
        .navigationTitle("New channel")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .topBarTrailing) {
                Button(busy ? "Creating..." : "Create") {
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
                guard let token = sessionTokenProvider() else {
                    errorMessage = "Sign in again to claim a handle, or leave it blank."
                    return
                }
                do {
                    _ = try await ChannelDiscovery.registerChannelHandle(
                        handle: wantedHandle,
                        channelIdHex: id,
                        sessionToken: token
                    )
                } catch let DiscoveryClientError.http(status, _, _) where status == 409 {
                    errorMessage = "@\(cleanHandle) is already taken. Pick another or leave it blank."
                    return
                } catch {
                    NSLog("Channels: handle registration failed: %@", String(describing: error))
                    errorMessage = "Could not claim the handle. Try again."
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
            dismiss()
        } catch let err as LegacyChannelError {
            errorMessage = err.message
        } catch {
            NSLog("Channels: create failed: %@", String(describing: error))
            errorMessage = "Could not create the channel. Try again."
        }
    }
}
