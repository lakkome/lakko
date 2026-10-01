// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import SwiftUI

/// The copy of the "Share my channels" switch (`docs/ux/feed.md` 6.3), from
/// the catalog (ADR-0061).
enum ShareChannelsCopy {
    static var label: String { Localization.text("discover.share.label") }
    static var line: String { Localization.text("discover.share.line") }
    static var failed: String { Localization.text("discover.share.error") }
}

/// "Share my channels with this group" in group info (`docs/ux/feed.md` 6.3,
/// ADR-0059 section 6). Mirror of web `discover/ShareChannelsSwitch.tsx`. Off
/// by default. Turning it on sends this person's listed channels to the
/// group, end-to-end encrypted; off sends an empty list. The switch changes
/// only once the send succeeded.
///
/// Not a block of the shared info rule (`lakko-ui-rules`), as on web: it is
/// shown in every group this device is still a member of.
struct ShareChannelsSection: View {
    @ObservedObject var hub: SharedChannelsHub
    let groupIdHex: String

    @State private var busy = false
    @State private var error: String?
    /// ADR-0061 — the switch follows the interface language.
    @ObservedObject private var l10n = Localization.shared

    var body: some View {
        Section {
            Toggle(ShareChannelsCopy.label, isOn: Binding(
                get: { hub.row.shareOn.contains(groupIdHex) },
                set: { next in Task { await toggle(next) } }))
                .disabled(busy)
            if let error {
                Text(error)
                    .font(.footnote)
                    .foregroundStyle(Color.lakkoDanger)
            }
        } footer: {
            Text(ShareChannelsCopy.line)
        }
        .onAppear { hub.bind() }
    }

    private func toggle(_ next: Bool) async {
        guard !busy else { return }
        busy = true
        error = nil
        defer { busy = false }
        do {
            try await hub.setSharing(groupIdHex: groupIdHex, on: next)
        } catch let e as SharedChannelsError {
            error = e.errorDescription
        } catch {
            self.error = ShareChannelsCopy.failed
        }
    }
}

/// Settings -> Security -> Channels I share (`docs/ux/feed.md` 6.3): every
/// group the "Share my channels" switch is on in, so it can be turned off in
/// one place. Turning it off sends the group an empty list. Mirror of web
/// `discover/ChannelsIShare.tsx`.
struct ChannelsIShareScreen: View {
    @ObservedObject var hub: SharedChannelsHub
    /// The group's title, or `nil` when this device no longer holds it.
    let title: (_ groupIdHex: String) -> String?

    @State private var busy: String?
    @State private var error: String?
    /// ADR-0061 — every line follows the interface language.
    @ObservedObject private var l10n = Localization.shared

    private var groups: [String] {
        hub.row.shareOn.sorted().filter { title($0) != nil }
    }

    var body: some View {
        List {
            Section {
                if groups.isEmpty {
                    Text(l10n.t("discover.shared.empty"))
                        .foregroundStyle(Color.lakkoTextMuted)
                } else {
                    ForEach(groups, id: \.self) { g in
                        HStack {
                            Text(title(g) ?? l10n.t("chat.group.fallback_name"))
                            Spacer()
                            Button(l10n.t("discover.shared.stop")) { Task { await stop(g) } }
                                .disabled(busy != nil)
                        }
                    }
                }
                if let error {
                    Text(error)
                        .font(.footnote)
                        .foregroundStyle(Color.lakkoDanger)
                }
            } footer: {
                Text(l10n.t("discover.shared.desc"))
            }
        }
        .navigationTitle(l10n.t("discover.shared.title"))
        .onAppear { hub.bind() }
    }

    private func stop(_ g: String) async {
        guard busy == nil else { return }
        busy = g
        error = nil
        defer { busy = nil }
        do {
            try await hub.setSharing(groupIdHex: g, on: false)
        } catch {
            self.error = Localization.text("discover.shared.stop_failed")
        }
    }
}
