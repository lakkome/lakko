// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore
import SwiftUI
import UIKit
import UserNotifications

// UX `docs/ux/identity-recovery.md` section 17.5 — "A device was added to
// your account", shown on every OTHER device of the account after a device
// joins it, by any route.
//
// The trigger is this account's own DeviceGroup roster gaining a device this
// device has not acknowledged. The DeviceGroup is the MLS-authenticated list
// of the account's devices, this device already follows it in the foreground
// (``DeviceGroupSyncRepository``), and it is what Settings > Devices lists;
// the directory's device list is served by a server and proves nothing.
//
//   * ``NewDeviceNoticeRecord``  what this device keeps, per account;
//   * ``NewDeviceNoticeRule``    the acknowledge / suppress rule, pure;
//   * ``NewDeviceNoticeStore``   where the record is kept (per device);
//   * ``NewDeviceNoticeCenter``  the published state the banners draw.

/// What this device keeps about the account's devices (UX 17.6). Devices are
/// named by the standard base64 of their DeviceGroup signature key, never by
/// a leaf index (a vacated leaf is handed to the next device added).
struct NewDeviceNoticeRecord: Codable, Equatable {

    /// A device this device has not acknowledged yet.
    struct Pending: Codable, Equatable {
        var key: String
        /// When THIS device learned of it (the roster carries no join date).
        var learnedAtMs: Int64
    }

    /// The devices already acknowledged: those present when this device
    /// joined, those it let in itself, and those dismissed.
    var acknowledged: [String] = []
    /// Devices this device let in (the code or the scan route) that have not
    /// shown up in the roster yet: no notice about them here.
    var letIn: [String] = []
    var pending: [Pending] = []
    /// A device that carried `New` was removed on this device: the
    /// change-password line takes the banner's place until dismissed.
    var removedNewDevice = false
}

/// The rule, pure.
enum NewDeviceNoticeRule {

    /// The roster was read. `record == nil` is this device's first reading
    /// for the account: every device present is known (a device never shows
    /// the notice about itself, and treats every device already in the
    /// account when it joined as known).
    static func observe(
        record: NewDeviceNoticeRecord?, roster: [String], ownKey: String?, nowMs: Int64
    ) -> NewDeviceNoticeRecord {
        guard var record else {
            return NewDeviceNoticeRecord(acknowledged: unique(roster))
        }
        let present = Set(roster)
        // A device that left ends its own notice.
        record.pending.removeAll { !present.contains($0.key) }
        var known = Set(record.acknowledged)
        let pendingKeys = Set(record.pending.map(\.key))
        for key in roster where !known.contains(key) && !pendingKeys.contains(key) {
            if key == ownKey || record.letIn.contains(key) {
                // This device itself, or one this device let in a moment
                // ago: a second alert about the same act is not shown.
                record.acknowledged.append(key)
                known.insert(key)
                record.letIn.removeAll { $0 == key }
            } else {
                record.pending.append(.init(key: key, learnedAtMs: nowMs))
            }
        }
        return record
    }

    /// This device let `key` in. Applies whether or not the roster already
    /// showed it.
    static func letIn(record: NewDeviceNoticeRecord?, key: String) -> NewDeviceNoticeRecord {
        var record = record ?? NewDeviceNoticeRecord()
        if record.pending.contains(where: { $0.key == key }) {
            record.pending.removeAll { $0.key == key }
            if !record.acknowledged.contains(key) { record.acknowledged.append(key) }
        } else if !record.acknowledged.contains(key), !record.letIn.contains(key) {
            record.letIn.append(key)
        }
        return record
    }

    /// `Dismiss`: every pending device is acknowledged; the `New` labels go
    /// with the banner. Opening Devices does NOT call this.
    static func dismiss(record: NewDeviceNoticeRecord) -> NewDeviceNoticeRecord {
        var record = record
        for entry in record.pending where !record.acknowledged.contains(entry.key) {
            record.acknowledged.append(entry.key)
        }
        record.pending = []
        return record
    }

    /// `key` was removed on this device. Removing a device that carried
    /// `New` ends its notice and raises the change-password line.
    static func removed(record: NewDeviceNoticeRecord, key: String) -> NewDeviceNoticeRecord {
        var record = record
        if record.pending.contains(where: { $0.key == key }) {
            record.pending.removeAll { $0.key == key }
            record.removedNewDevice = true
        }
        record.acknowledged.removeAll { $0 == key }
        record.letIn.removeAll { $0 == key }
        return record
    }

    /// The change-password line was dismissed.
    static func dismissRemovedLine(record: NewDeviceNoticeRecord) -> NewDeviceNoticeRecord {
        var record = record
        record.removedNewDevice = false
        return record
    }

    /// What the banner says, or `nil` when there is none.
    struct Banner: Equatable {
        /// How many devices are new (`title` for one, `title_many` above).
        var count: Int
        /// The most recent join time known.
        var latestMs: Int64

        var titleKey: String {
            count > 1 ? "devices.added_notice.title_many" : "devices.added_notice.title"
        }
    }

    static func banner(record: NewDeviceNoticeRecord?) -> Banner? {
        guard let record, let latest = record.pending.map(\.learnedAtMs).max() else { return nil }
        return Banner(count: record.pending.count, latestMs: latest)
    }

    /// Whether the row of `key` carries `New`.
    static func isNew(record: NewDeviceNoticeRecord?, key: String) -> Bool {
        record?.pending.contains { $0.key == key } ?? false
    }

    /// Whether the roster reading added a pending device (the local
    /// notification's trigger).
    static func gainedPending(before: NewDeviceNoticeRecord?, after: NewDeviceNoticeRecord) -> Bool {
        let had = Set(before?.pending.map(\.key) ?? [])
        return after.pending.contains { !had.contains($0.key) }
    }

    private static func unique(_ keys: [String]) -> [String] {
        var seen = Set<String>()
        return keys.filter { seen.insert($0).inserted }
    }
}

/// The record's storage: per device, per account, in `UserDefaults`. It
/// holds device signature keys (public) and times, nothing secret. Removed
/// with the account by ``AccountLocalDataWipe``.
struct NewDeviceNoticeStore {
    var defaults: UserDefaults = .standard

    static let keyPrefix = "me.lakko.new-device-notice.v1."

    static func key(accountIdB64: String) -> String { keyPrefix + accountIdB64 }

    func load(accountIdB64: String) -> NewDeviceNoticeRecord? {
        guard let data = defaults.data(forKey: Self.key(accountIdB64: accountIdB64)) else { return nil }
        return try? JSONDecoder().decode(NewDeviceNoticeRecord.self, from: data)
    }

    func save(_ record: NewDeviceNoticeRecord, accountIdB64: String) {
        guard let gate = AccountStoreGate.enterUnlessWiped(accountIdB64) else { return }
        defer { gate.leave() }
        guard let data = try? JSONEncoder().encode(record) else { return }
        defaults.set(data, forKey: Self.key(accountIdB64: accountIdB64))
    }

    func clear(accountIdB64: String) {
        defaults.removeObject(forKey: Self.key(accountIdB64: accountIdB64))
    }
}

/// The notice's published state. One per process; the DeviceGroup sync
/// runtime feeds it every roster it reads, the add-device routes tell it
/// which device this one let in, and the banners draw from it.
@MainActor
final class NewDeviceNoticeCenter: ObservableObject {

    static let shared = NewDeviceNoticeCenter()

    @Published private(set) var record: NewDeviceNoticeRecord?
    /// A tap on the local notification asked for Settings > Devices.
    @Published var openDevicesRequested = false

    private let store: NewDeviceNoticeStore
    private let nowMs: () -> Int64
    /// Posts the local notification when the app is running but not in
    /// front. Injectable for tests.
    private let notifyIfNotInFront: @MainActor () -> Void
    private var accountIdB64: String?

    init(
        store: NewDeviceNoticeStore = NewDeviceNoticeStore(),
        nowMs: @escaping () -> Int64 = { Int64(Date().timeIntervalSince1970 * 1000) },
        notifyIfNotInFront: @escaping @MainActor () -> Void = NewDeviceNoticeCenter.postLocalNotificationIfNotInFront
    ) {
        self.store = store
        self.nowMs = nowMs
        self.notifyIfNotInFront = notifyIfNotInFront
    }

    var banner: NewDeviceNoticeRule.Banner? { NewDeviceNoticeRule.banner(record: record) }
    var showsRemovedLine: Bool { record?.removedNewDevice ?? false }

    func isNew(signaturePublicKey: Data) -> Bool {
        NewDeviceNoticeRule.isNew(record: record, key: signaturePublicKey.base64EncodedString())
    }

    /// The account whose record is shown; `nil` on sign-out.
    func bind(accountIdB64: String?) {
        guard accountIdB64 != self.accountIdB64 else { return }
        self.accountIdB64 = accountIdB64
        record = accountIdB64.flatMap { store.load(accountIdB64: $0) }
    }

    /// The DeviceGroup roster of `accountIdB64` as just read.
    func observe(accountIdB64: String, roster: [Data], ownKey: Data?) {
        bind(accountIdB64: accountIdB64)
        let before = record
        let after = NewDeviceNoticeRule.observe(
            record: before,
            roster: roster.map { $0.base64EncodedString() },
            ownKey: ownKey?.base64EncodedString(),
            nowMs: nowMs())
        guard after != before else { return }
        commit(after)
        if NewDeviceNoticeRule.gainedPending(before: before, after: after) {
            notifyIfNotInFront()
        }
    }

    /// This device let the device with `signaturePublicKey` in.
    func letIn(accountIdB64: String, signaturePublicKey: Data) {
        bind(accountIdB64: accountIdB64)
        commit(NewDeviceNoticeRule.letIn(record: record, key: signaturePublicKey.base64EncodedString()))
    }

    func dismiss() {
        guard let record else { return }
        commit(NewDeviceNoticeRule.dismiss(record: record))
    }

    /// The device with `signaturePublicKey` was removed on this device.
    func removed(signaturePublicKey: Data) {
        guard let record else { return }
        commit(NewDeviceNoticeRule.removed(record: record, key: signaturePublicKey.base64EncodedString()))
    }

    func dismissRemovedLine() {
        guard let record else { return }
        commit(NewDeviceNoticeRule.dismissRemovedLine(record: record))
    }

    private func commit(_ next: NewDeviceNoticeRecord) {
        guard let accountIdB64 else { return }
        record = next
        store.save(next, accountIdB64: accountIdB64)
    }

    // MARK: - Local notification

    /// The `userInfo` key a tap is recognised by.
    static let notificationRouteKey = "lakko.route"
    static let notificationRouteDevices = "devices"

    /// Post the local notification when the app is running but not in front
    /// (UX 17.5). It names no device and no place. When the app is in front
    /// the banner is enough; when it is closed nothing runs here at all.
    static func postLocalNotificationIfNotInFront() {
        guard UIApplication.shared.applicationState != .active else { return }
        let content = UNMutableNotificationContent()
        content.title = Localization.text("devices.added_notice.title")
        content.body = Localization.text("devices.added_notice.notification_body")
        content.userInfo = [notificationRouteKey: notificationRouteDevices]
        let request = UNNotificationRequest(
            identifier: "me.lakko.new-device-notice", content: content, trigger: nil)
        UNUserNotificationCenter.current().add(request) { error in
            if error != nil { NSLog("[devices] the new-device notification was not posted") }
        }
    }
}

/// The banner of UX 17.5: icon, text, one action, `Dismiss`. Not a modal; it
/// never takes focus by itself. Observes the center itself so the screen
/// that hosts it is not re-evaluated by it.
struct NewDeviceNoticeBanner: View {
    @ObservedObject var center: NewDeviceNoticeCenter
    /// `Open Devices`; `nil` on Settings > Devices itself.
    var onOpenDevices: (() -> Void)?

    @ObservedObject private var l10n = Localization.shared

    static func when(_ ms: Int64, locale: Locale) -> String {
        Date(timeIntervalSince1970: TimeInterval(ms) / 1000)
            .formatted(.dateTime.year().month(.abbreviated).day().hour().minute().locale(locale))
    }

    var body: some View {
        if let banner = center.banner {
            let title = l10n.t(banner.titleKey)
            let when = l10n.t(
                "devices.added_notice.when",
                texts: ["date": Self.when(banner.latestMs, locale: l10n.locale)])
            let notYou = l10n.t("devices.added_notice.not_you")
            HStack(alignment: .top, spacing: 12) {
                Image(systemName: "laptopcomputer.and.iphone")
                    .font(.subheadline)
                    .foregroundStyle(Color.lakkoAccent)
                    .accessibilityHidden(true)
                VStack(alignment: .leading, spacing: 4) {
                    VStack(alignment: .leading, spacing: 4) {
                        Text(title)
                            .font(.subheadline.weight(.semibold))
                            .foregroundStyle(Color.lakkoText)
                        Text(when)
                            .font(.footnote)
                            .foregroundStyle(Color.lakkoTextMuted)
                        Text(notYou)
                            .font(.footnote)
                            .foregroundStyle(Color.lakkoTextMuted)
                    }
                    // Title and both lines are one announcement.
                    .accessibilityElement(children: .ignore)
                    .accessibilityLabel("\(title) \(when) \(notYou)")
                    HStack(spacing: 16) {
                        if let onOpenDevices {
                            Button(l10n.t("devices.added_notice.open_devices"), action: onOpenDevices)
                                .font(.footnote.weight(.semibold))
                                .frame(minHeight: 44)
                        }
                        Button(l10n.t("common.dismiss")) { center.dismiss() }
                            .font(.footnote)
                            .frame(minWidth: 44, minHeight: 44)
                            .contentShape(Rectangle())
                    }
                    .frame(maxWidth: .infinity, alignment: .trailing)
                }
            }
            .padding(12)
            .background(Color.lakkoAccentSoft)
            .overlay(
                RoundedRectangle(cornerRadius: LakkoRadius.control)
                    .stroke(Color.lakkoAccent.opacity(0.5), lineWidth: 1)
            )
            .padding(8)
            .fixedSize(horizontal: false, vertical: true)
            .environment(\.layoutDirection, l10n.layoutDirection)
            .accessibilityElement(children: .contain)
            .onAppear {
                // Announced once, politely: it does not interrupt typing.
                var announcement = AttributedString("\(title) \(when) \(notYou)")
                announcement.accessibilitySpeechAnnouncementPriority = .low
                AccessibilityNotification.Announcement(announcement).post()
            }
        } else if center.showsRemovedLine {
            HStack(alignment: .top, spacing: 12) {
                Image(systemName: "exclamationmark.shield")
                    .font(.subheadline)
                    .foregroundStyle(Color.lakkoWarning)
                    .accessibilityHidden(true)
                VStack(alignment: .leading, spacing: 4) {
                    Text(l10n.t("settings.devices.removed_change_password"))
                        .font(.subheadline)
                        .foregroundStyle(Color.lakkoText)
                    HStack(spacing: 16) {
                        if let onChangePassword {
                            Button(l10n.t("settings.password.change"), action: onChangePassword)
                                .font(.footnote.weight(.semibold))
                                .frame(minHeight: 44)
                        }
                        Button(l10n.t("common.dismiss")) { center.dismissRemovedLine() }
                            .font(.footnote)
                            .frame(minWidth: 44, minHeight: 44)
                            .contentShape(Rectangle())
                    }
                    .frame(maxWidth: .infinity, alignment: .trailing)
                }
            }
            .padding(12)
            .background(Color.lakkoSurface)
            .overlay(
                RoundedRectangle(cornerRadius: LakkoRadius.control)
                    .stroke(Color.lakkoBorder, lineWidth: 1)
            )
            .padding(8)
            .fixedSize(horizontal: false, vertical: true)
            .environment(\.layoutDirection, l10n.layoutDirection)
        }
    }

    /// `Change password` on the removed line; `nil` hides the button.
    var onChangePassword: (() -> Void)?
}
