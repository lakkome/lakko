// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore
import SwiftUI
import UIKit
import UserNotifications

// UX `docs/ux/identity-recovery.md` sections 17.5 / 17.6 — "A device was
// added to your account", shown on every OTHER device of the account after a
// device joins it, by any route: a system notification, an exclamation-mark
// badge on Settings and on its Devices row, and a highlighted row in
// Settings > Devices. Looking at that screen on any device clears it on all
// of them.
//
// The trigger is this account's own DeviceGroup roster gaining a device this
// device does not treat as known. The DeviceGroup is the MLS-authenticated
// list of the account's devices, this device already follows it in the
// foreground (``DeviceGroupSyncRepository``), and it is what Settings >
// Devices lists; the directory's device list is served by a server and
// proves nothing.
//
// A device is PENDING here when it is in the roster, this device does not
// treat it as known, and no device of the account has acknowledged it (the
// synced `lakko-sync` device-acknowledgement register, held by the sync
// repository and handed to the center as a set of keys).
//
//   * ``NewDeviceNoticeRecord``  what this device keeps, per account;
//   * ``NewDeviceNoticeRule``    the rule, pure;
//   * ``NewDeviceNoticeStore``   where the record is kept (per device);
//   * ``NewDeviceNoticeCenter``  the published state the surfaces draw.

/// What this device keeps about the account's devices (UX 17.6). Devices are
/// named by the standard base64 of their DeviceGroup signature key, never by
/// a leaf index (a vacated leaf is handed to the next device added).
struct NewDeviceNoticeRecord: Codable, Equatable {

    /// A device this device does not treat as known yet.
    struct Pending: Codable, Equatable {
        var key: String
        /// When THIS device learned of it (the roster carries no join date).
        var learnedAtMs: Int64
    }

    /// The devices this device treats as known: those present when it
    /// joined, those it let in itself, and those acknowledged by any device
    /// of the account.
    var known: [String] = []
    /// Devices this device let in (the code or the scan route) that have not
    /// shown up in the roster yet: no warning about them here.
    var letIn: [String] = []
    var pending: [Pending] = []
    /// A device that carried `New` was removed on this device: the
    /// change-password line is shown in Settings > Devices until dismissed.
    var removedNewDevice = false
}

/// The rule, pure. `acknowledged` is always the synced set of acknowledged
/// device keys (standard base64), from whichever device it came.
enum NewDeviceNoticeRule {

    /// The roster was read. `record == nil` is this device's first reading
    /// for the account: every device present is known (a device never warns
    /// about itself, and treats every device already in the account when it
    /// joined as known). A device acknowledged elsewhere before this device
    /// learned of it is known at once and never pending.
    static func observe(
        record: NewDeviceNoticeRecord?, roster: [String], ownKey: String?,
        acknowledged: Set<String>, nowMs: Int64
    ) -> NewDeviceNoticeRecord {
        guard var record else {
            return NewDeviceNoticeRecord(known: unique(roster))
        }
        let present = Set(roster)
        // A device that left ends its own warning.
        record.pending.removeAll { !present.contains($0.key) }
        var known = Set(record.known)
        let pendingKeys = Set(record.pending.map(\.key))
        for key in roster where !known.contains(key) && !pendingKeys.contains(key) {
            if key == ownKey || record.letIn.contains(key) || acknowledged.contains(key) {
                // This device itself, one this device let in a moment ago
                // (a second alert about the same act is not shown), or one
                // already seen on another device of the account.
                record.known.append(key)
                known.insert(key)
                record.letIn.removeAll { $0 == key }
            } else {
                record.pending.append(.init(key: key, learnedAtMs: nowMs))
            }
        }
        return absorb(record: record, acknowledged: acknowledged)
    }

    /// The acknowledged set grew: the acknowledged devices move from pending
    /// to known on this device.
    static func absorb(record: NewDeviceNoticeRecord, acknowledged: Set<String>) -> NewDeviceNoticeRecord {
        var record = record
        for entry in record.pending where acknowledged.contains(entry.key) {
            if !record.known.contains(entry.key) { record.known.append(entry.key) }
        }
        record.pending.removeAll { acknowledged.contains($0.key) }
        return record
    }

    /// The devices pending here: the record's pending set minus every
    /// acknowledged device.
    static func pending(record: NewDeviceNoticeRecord?, acknowledged: Set<String>) -> [NewDeviceNoticeRecord.Pending] {
        (record?.pending ?? []).filter { !acknowledged.contains($0.key) }
    }

    /// Whether Settings and its Devices row carry the badge.
    static func badgeVisible(record: NewDeviceNoticeRecord?, acknowledged: Set<String>) -> Bool {
        !pending(record: record, acknowledged: acknowledged).isEmpty
    }

    /// This device let `key` in. Applies whether or not the roster already
    /// showed it. It does not acknowledge the device for the account.
    static func letIn(record: NewDeviceNoticeRecord?, key: String) -> NewDeviceNoticeRecord {
        var record = record ?? NewDeviceNoticeRecord()
        if record.pending.contains(where: { $0.key == key }) {
            record.pending.removeAll { $0.key == key }
            if !record.known.contains(key) { record.known.append(key) }
        } else if !record.known.contains(key), !record.letIn.contains(key) {
            record.letIn.append(key)
        }
        return record
    }

    /// `key` was removed on this device. Removing a device that carried
    /// `New` (pending here, or highlighted in the visit in progress) ends
    /// its warning and raises the change-password line.
    static func removed(record: NewDeviceNoticeRecord, key: String, wasHighlighted: Bool) -> NewDeviceNoticeRecord {
        var record = record
        if record.pending.contains(where: { $0.key == key }) || wasHighlighted {
            record.pending.removeAll { $0.key == key }
            record.removedNewDevice = true
        }
        record.known.removeAll { $0 == key }
        record.letIn.removeAll { $0 == key }
        return record
    }

    /// The change-password line was dismissed.
    static func dismissRemovedLine(record: NewDeviceNoticeRecord) -> NewDeviceNoticeRecord {
        var record = record
        record.removedNewDevice = false
        return record
    }

    /// A visit to Settings > Devices that started with something pending:
    /// the devices to acknowledge for the account, and the rows this visit
    /// keeps highlighted until the screen is left.
    struct Visit: Equatable {
        var keysToAcknowledge: [String]
        var highlighted: [NewDeviceNoticeRecord.Pending]
    }

    /// Settings > Devices was opened. `nil` when nothing is pending (the
    /// visit then highlights nothing and acknowledges nothing).
    static func openDevices(record: NewDeviceNoticeRecord?, acknowledged: Set<String>) -> Visit? {
        let pending = pending(record: record, acknowledged: acknowledged)
        guard !pending.isEmpty else { return nil }
        return Visit(keysToAcknowledge: pending.map(\.key), highlighted: pending)
    }

    /// What the system notification does after a change of the pending set.
    enum NotificationAction: Equatable {
        case none
        /// Post (or replace) it, titled for `count` pending devices.
        case post(count: Int)
        /// Withdraw it: nothing is pending any more.
        case withdraw
    }

    /// The notification follows the pending set: posted when it gained a
    /// device, in front or not; withdrawn when it became empty.
    static func notification(before: [String], after: [String]) -> NotificationAction {
        let had = Set(before)
        if after.contains(where: { !had.contains($0) }) { return .post(count: after.count) }
        if after.isEmpty, !before.isEmpty { return .withdraw }
        return .none
    }

    /// The explanation above the list in Settings > Devices.
    struct Explanation: Equatable {
        /// How many devices are new (`title` for one, `title_many` above).
        var count: Int
        /// The most recent join time known.
        var latestMs: Int64

        var titleKey: String { NewDeviceNoticeRule.titleKey(count: count) }
    }

    static func explanation(_ pending: [NewDeviceNoticeRecord.Pending]) -> Explanation? {
        guard let latest = pending.map(\.learnedAtMs).max() else { return nil }
        return Explanation(count: pending.count, latestMs: latest)
    }

    static func titleKey(count: Int) -> String {
        count > 1 ? "devices.added_notice.title_many" : "devices.added_notice.title"
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

    static let keyPrefix = "me.lakko.new-device-notice.v2."

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

/// Posts and withdraws the system notification. Injectable for tests.
protocol NewDeviceNotifier {
    @MainActor func post(accountIdB64: String, count: Int)
    @MainActor func withdraw(accountIdB64: String)
}

/// The system notification through `UNUserNotificationCenter`, under the
/// app's existing notification permission (never asked for here). One fixed
/// identifier per account, so a second device replaces the first
/// notification instead of stacking.
struct SystemNewDeviceNotifier: NewDeviceNotifier {
    func post(accountIdB64: String, count: Int) {
        let content = UNMutableNotificationContent()
        content.title = Localization.text(NewDeviceNoticeRule.titleKey(count: count))
        content.body = Localization.text("devices.added_notice.notification_body")
        content.userInfo = [
            NewDeviceNoticeCenter.notificationRouteKey: NewDeviceNoticeCenter.notificationRouteDevices,
        ]
        let request = UNNotificationRequest(
            identifier: NewDeviceNoticeCenter.notificationIdentifier(accountIdB64: accountIdB64),
            content: content, trigger: nil)
        UNUserNotificationCenter.current().add(request) { error in
            if error != nil { NSLog("[devices] the new-device notification was not posted") }
        }
    }

    func withdraw(accountIdB64: String) {
        let ids = [NewDeviceNoticeCenter.notificationIdentifier(accountIdB64: accountIdB64)]
        let center = UNUserNotificationCenter.current()
        center.removePendingNotificationRequests(withIdentifiers: ids)
        center.removeDeliveredNotifications(withIdentifiers: ids)
    }
}

/// The warning's published state. One per process; the DeviceGroup sync
/// runtime feeds it every roster it reads and the synced acknowledged set,
/// and takes its acknowledgements; the add-device routes tell it which
/// device this one let in; the badges, the Devices screen and the
/// notification draw from it.
@MainActor
final class NewDeviceNoticeCenter: ObservableObject {

    static let shared = NewDeviceNoticeCenter()

    @Published private(set) var record: NewDeviceNoticeRecord?
    /// The synced acknowledged set (standard base64 keys).
    @Published private(set) var acknowledged: Set<String> = []
    /// The visit to Settings > Devices in progress, when it started with
    /// something pending (or something became pending during it).
    @Published private(set) var visit: NewDeviceNoticeRule.Visit?
    /// A tap on the notification asked for Settings > Devices.
    @Published var openDevicesRequested = false

    private let store: NewDeviceNoticeStore
    private let nowMs: () -> Int64
    private let notifier: NewDeviceNotifier
    private var accountIdB64: String?
    /// Settings > Devices is on screen.
    private var visiting = false
    /// Writes the synced acknowledgement for these keys (32 bytes each) and
    /// sends it to the account's other devices. Bound by the sync runtime
    /// once its synced state is ready; until then a visit acknowledges
    /// nothing, so nothing pending is consumed on this device alone.
    private var acknowledger: (([Data]) -> Void)?

    init(
        store: NewDeviceNoticeStore = NewDeviceNoticeStore(),
        nowMs: @escaping () -> Int64 = { Int64(Date().timeIntervalSince1970 * 1000) },
        notifier: NewDeviceNotifier = SystemNewDeviceNotifier()
    ) {
        self.store = store
        self.nowMs = nowMs
        self.notifier = notifier
    }

    /// The devices pending here.
    var pending: [NewDeviceNoticeRecord.Pending] {
        NewDeviceNoticeRule.pending(record: record, acknowledged: acknowledged)
    }

    /// Settings and its Devices row carry the badge.
    var badgeVisible: Bool { !pending.isEmpty }

    /// The explanation above the list in Settings > Devices: the devices this
    /// visit highlights.
    var explanation: NewDeviceNoticeRule.Explanation? {
        NewDeviceNoticeRule.explanation(highlightedEntries)
    }

    var showsRemovedLine: Bool { record?.removedNewDevice ?? false }

    /// Whether the row of `signaturePublicKey` is highlighted and carries
    /// `New`: highlighted by this visit, or pending here.
    func isHighlighted(signaturePublicKey: Data) -> Bool {
        let key = signaturePublicKey.base64EncodedString()
        return highlightedEntries.contains { $0.key == key }
    }

    private var highlightedEntries: [NewDeviceNoticeRecord.Pending] {
        var entries = visit?.highlighted ?? []
        for entry in pending where !entries.contains(where: { $0.key == entry.key }) {
            entries.append(entry)
        }
        return entries
    }

    /// The account whose record is shown; `nil` on sign-out.
    func bind(accountIdB64: String?) {
        guard accountIdB64 != self.accountIdB64 else { return }
        self.accountIdB64 = accountIdB64
        record = accountIdB64.flatMap { store.load(accountIdB64: $0) }
        acknowledged = []
        visit = nil
        if accountIdB64 == nil { acknowledger = nil }
    }

    /// The sync runtime of `accountIdB64` (its synced state now ready)
    /// writes and sends acknowledgements through `acknowledger`; `nil`
    /// unbinds it. When Settings > Devices is already open, what is pending
    /// is acknowledged now.
    func setAcknowledger(accountIdB64: String, _ acknowledger: (([Data]) -> Void)?) {
        bind(accountIdB64: accountIdB64)
        self.acknowledger = acknowledger
        if visiting { acknowledgePendingForVisit() }
    }

    /// The synced acknowledged set of `accountIdB64`, as just folded.
    func setAcknowledged(accountIdB64: String, keys: [Data]) {
        bind(accountIdB64: accountIdB64)
        let next = Set(keys.map { $0.base64EncodedString() }).union(acknowledged)
        guard next != acknowledged else { return }
        apply(record: record.map { NewDeviceNoticeRule.absorb(record: $0, acknowledged: next) },
              acknowledged: next)
    }

    /// The DeviceGroup roster of `accountIdB64` as just read.
    func observe(accountIdB64: String, roster: [Data], ownKey: Data?) {
        bind(accountIdB64: accountIdB64)
        let next = NewDeviceNoticeRule.observe(
            record: record,
            roster: roster.map { $0.base64EncodedString() },
            ownKey: ownKey?.base64EncodedString(),
            acknowledged: acknowledged,
            nowMs: nowMs())
        apply(record: next, acknowledged: acknowledged)
    }

    /// This device let the device with `signaturePublicKey` in.
    func letIn(accountIdB64: String, signaturePublicKey: Data) {
        bind(accountIdB64: accountIdB64)
        apply(record: NewDeviceNoticeRule.letIn(record: record, key: signaturePublicKey.base64EncodedString()),
              acknowledged: acknowledged)
    }

    /// The device with `signaturePublicKey` was removed on this device.
    func removed(signaturePublicKey: Data) {
        guard let record else { return }
        let highlighted = isHighlighted(signaturePublicKey: signaturePublicKey)
        let key = signaturePublicKey.base64EncodedString()
        if var visit {
            visit.highlighted.removeAll { $0.key == key }
            self.visit = visit
        }
        apply(record: NewDeviceNoticeRule.removed(record: record, key: key, wasHighlighted: highlighted),
              acknowledged: acknowledged)
    }

    func dismissRemovedLine() {
        guard let record else { return }
        apply(record: NewDeviceNoticeRule.dismissRemovedLine(record: record), acknowledged: acknowledged)
    }

    /// Settings > Devices came on screen: every device pending here is
    /// acknowledged for the account; this visit keeps them highlighted.
    func beginVisit() {
        visiting = true
        acknowledgePendingForVisit()
    }

    /// Settings > Devices left the screen: the highlight and the
    /// explanation go.
    func endVisit() {
        visiting = false
        visit = nil
    }

    /// While the screen is up, a device that becomes pending is in front of
    /// the person too: it is acknowledged and joins the visit's highlight.
    private func acknowledgePendingForVisit() {
        // Before the synced state is ready nothing is acknowledged, here or
        // for the account: the rows stay pending (and highlighted).
        guard let acknowledger else { return }
        guard let opened = NewDeviceNoticeRule.openDevices(record: record, acknowledged: acknowledged) else {
            return
        }
        var current = visit ?? .init(keysToAcknowledge: [], highlighted: [])
        for entry in opened.highlighted where !current.highlighted.contains(where: { $0.key == entry.key }) {
            current.highlighted.append(entry)
        }
        current.keysToAcknowledge.append(contentsOf: opened.keysToAcknowledge)
        visit = current
        // Acknowledged here at once, so the badge and the notification go
        // without waiting for the sync runtime to fold it.
        let next = acknowledged.union(opened.keysToAcknowledge)
        apply(record: record.map { NewDeviceNoticeRule.absorb(record: $0, acknowledged: next) },
              acknowledged: next, fromVisit: true)
        acknowledger(opened.keysToAcknowledge.compactMap { Data(base64Encoded: $0) })
    }

    /// The one path every change goes through: persist, then make the
    /// notification follow the pending set.
    private func apply(record next: NewDeviceNoticeRecord?, acknowledged nextAcknowledged: Set<String>, fromVisit: Bool = false) {
        guard let accountIdB64 else { return }
        let before = pending.map(\.key)
        if next != record, let next {
            record = next
            store.save(next, accountIdB64: accountIdB64)
        }
        if nextAcknowledged != acknowledged { acknowledged = nextAcknowledged }
        if visiting, !fromVisit, acknowledger != nil, !pending.isEmpty {
            acknowledgePendingForVisit()
            return
        }
        switch NewDeviceNoticeRule.notification(before: before, after: pending.map(\.key)) {
        case .none: break
        case .post(let count): notifier.post(accountIdB64: accountIdB64, count: count)
        case .withdraw: notifier.withdraw(accountIdB64: accountIdB64)
        }
    }

    // MARK: - The system notification

    /// The `userInfo` key a tap is recognised by.
    nonisolated static let notificationRouteKey = "lakko.route"
    nonisolated static let notificationRouteDevices = "devices"

    /// The fixed identifier of the account's notification.
    nonisolated static func notificationIdentifier(accountIdB64: String) -> String {
        "me.lakko.new-device-notice." + accountIdB64
    }
}

/// The exclamation-mark badge of UX 17.5, in the warning colour (yellow,
/// maintainer 2026-10-29) and never colour alone: the caller gives the
/// element its accessible name.
struct NewDeviceAttentionBadge: View {
    var body: some View {
        Image(systemName: "exclamationmark.circle.fill")
            .font(.body)
            .foregroundStyle(Color.lakkoWarning)
            .accessibilityHidden(true)
    }
}

/// The tab-bar badge colour (UX 17.5): the Settings tab's "!" is the only
/// tab badge Lakko draws, and SwiftUI's `.badge` has no colour of its own,
/// so the tab bar's item appearance carries the warning colour with
/// readable text on it in both appearances. Installed once, before the tab
/// bar exists.
enum NewDeviceAttentionTabBadge {
    @MainActor
    static func installAppearance() {
        let item = UITabBarItem.appearance()
        item.badgeColor = UIColor(named: "LakkoWarning")
        let text = UIColor(named: "LakkoAccentForeground") ?? .white
        item.setBadgeTextAttributes([.foregroundColor: text], for: .normal)
        item.setBadgeTextAttributes([.foregroundColor: text], for: .selected)
    }
}

/// The explanation above the list in Settings > Devices (UX 17.5): icon,
/// title, when, what to do. No button: opening this screen is the
/// acknowledgement. Read as one announcement; it never takes focus by
/// itself. After a `New` device was removed here, the change-password line
/// takes its place.
struct NewDeviceNoticeExplanation: View {
    @ObservedObject var center: NewDeviceNoticeCenter
    /// `Change password` on the removed line.
    var onChangePassword: (() -> Void)?

    @ObservedObject private var l10n = Localization.shared

    static func when(_ ms: Int64, locale: Locale) -> String {
        Date(timeIntervalSince1970: TimeInterval(ms) / 1000)
            .formatted(.dateTime.year().month(.abbreviated).day().hour().minute().locale(locale))
    }

    var body: some View {
        if let explanation = center.explanation {
            let title = l10n.t(explanation.titleKey)
            let when = l10n.t(
                "devices.added_notice.when",
                texts: ["date": Self.when(explanation.latestMs, locale: l10n.locale)])
            let notYou = l10n.t("devices.added_notice.not_you")
            HStack(alignment: .top, spacing: 12) {
                Image(systemName: "laptopcomputer.and.iphone")
                    .font(.subheadline)
                    .foregroundStyle(Color.lakkoAccent)
                    .accessibilityHidden(true)
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
                .frame(maxWidth: .infinity, alignment: .leading)
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
            // Title and both lines are one announcement.
            .accessibilityElement(children: .ignore)
            .accessibilityLabel("\(title) \(when) \(notYou)")
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
}
