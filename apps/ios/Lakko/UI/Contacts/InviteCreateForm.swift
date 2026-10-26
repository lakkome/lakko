// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// The terms form of the Invite links sheet, as pure data. Since
/// `docs/ux/contact-invite-link.md` section 9 (2026-10-26) it is the EDIT form
/// of one link (**Create link** mints the defaults, ``InviteCreateForm()``,
/// with no form), prefilled by ``prefilled(label:expiryMs:maxUses:mintedAtMs:)``.
/// The presets, the custom-field validation and the mapping to what
/// ``ContactsRepository/generateInviteLink(label:expiry:maxUses:bootstrapReferral:)``
/// takes. The same choices and bounds as the web `invite.ts`
/// (`expiryPresetToMs`, `customExpiryMs`, `customMaxUses`) and Android
/// `InviteCreateForm.kt`.
struct InviteCreateForm: Equatable {
    enum Expiry: Hashable, CaseIterable { case oneDay, sevenDays, thirtyDays, never, custom }
    enum ExpiryUnit: Hashable, CaseIterable { case hours, days }
    enum Uses: Hashable, CaseIterable { case one, upToTen, unlimited, custom }

    /// Hard cap on the label, in UTF-8 bytes. Mirrors
    /// `lakko_contacts::invite::MAX_LABEL_LEN` (and the web
    /// `INVITE_LABEL_MAX_LEN`); a change to the Rust constant changes this one.
    static let labelMaxBytes = 64
    static let hourMs: Int64 = 3_600_000
    static let dayMs: Int64 = 24 * hourMs
    /// The longest custom period a link may be given: 365 days. Chosen, not
    /// measured; the same bound on web and Android.
    static let customExpiryMaxMs: Int64 = 365 * dayMs
    /// The largest custom number of people a link may admit. Chosen, not
    /// measured; the same bound on web and Android.
    static let customMaxUses: UInt32 = 10_000
    /// Wire `max_uses` sentinels as `lakko-contacts` defines them.
    static let singleUseWire: UInt32 = ContactsRepository.singleUse
    static let unlimitedUsesWire: UInt32 = UInt32.max

    var label = ""
    var expiry: Expiry = .sevenDays
    var customExpiryAmount = ""
    var customExpiryUnit: ExpiryUnit = .days
    var uses: Uses = .one
    var customUses = ""

    /// What a valid form creates.
    struct Params: Equatable {
        let label: String
        let expiry: InviteExpiry
        let maxUses: UInt32
    }

    /// The custom period in milliseconds, or `nil` when `amount` is not a
    /// whole number or the period is outside 1 hour .. 365 days.
    static func customExpiryMs(amount: String, unit: ExpiryUnit) -> Int64? {
        guard let n = parseWholeNumber(amount) else { return nil }
        let ms = n * (unit == .hours ? hourMs : dayMs)
        return (hourMs...customExpiryMaxMs).contains(ms) ? ms : nil
    }

    /// The wire `max_uses` for a custom number of people, or `nil` when
    /// `count` is not a whole number in 1 ... ``customMaxUses``. One person is
    /// the single-use sentinel `0`, as the preset writes it.
    static func customMaxUsesWire(count: String) -> UInt32? {
        guard let n = parseWholeNumber(count), n >= 1, n <= Int64(customMaxUses) else { return nil }
        return n == 1 ? singleUseWire : UInt32(n)
    }

    /// Show the custom-expiry error: Custom is chosen and the field holds
    /// text that is not a valid period (an empty field only disables Create).
    var showsCustomExpiryError: Bool {
        expiry == .custom
            && !customExpiryAmount.trimmingCharacters(in: .whitespaces).isEmpty
            && Self.customExpiryMs(amount: customExpiryAmount, unit: customExpiryUnit) == nil
    }

    /// Show the custom-uses error, by the same rule.
    var showsCustomUsesError: Bool {
        uses == .custom
            && !customUses.trimmingCharacters(in: .whitespaces).isEmpty
            && Self.customMaxUsesWire(count: customUses) == nil
    }

    /// The parameters the form creates, or `nil` while it is not valid
    /// (Create is disabled).
    var params: Params? {
        let expiryChoice: InviteExpiry
        switch expiry {
        case .oneDay: expiryChoice = .fromNow(ms: 1 * Self.dayMs)
        case .sevenDays: expiryChoice = .fromNow(ms: 7 * Self.dayMs)
        case .thirtyDays: expiryChoice = .fromNow(ms: 30 * Self.dayMs)
        case .never: expiryChoice = .never
        case .custom:
            guard let ms = Self.customExpiryMs(amount: customExpiryAmount, unit: customExpiryUnit)
            else { return nil }
            expiryChoice = .fromNow(ms: ms)
        }
        let maxUses: UInt32
        switch uses {
        case .one: maxUses = Self.singleUseWire
        case .upToTen: maxUses = 10
        case .unlimited: maxUses = Self.unlimitedUsesWire
        case .custom:
            guard let wire = Self.customMaxUsesWire(count: customUses) else { return nil }
            maxUses = wire
        }
        let trimmed = label.trimmingCharacters(in: .whitespacesAndNewlines)
        guard trimmed.utf8.count <= Self.labelMaxBytes else { return nil }
        return Params(label: trimmed, expiry: expiryChoice, maxUses: maxUses)
    }

    /// How close a link's lifetime must be to a preset (1, 7 or 30 days) to
    /// read back as that preset: one minute, the slack between the moment the
    /// form asked for "7 days from now" and the moment the core stamped
    /// `mintedAtMs`.
    static let presetToleranceMs: Int64 = 60_000

    /// The edit form of an existing link (`docs/ux/contact-invite-link.md`
    /// section 9), prefilled with its terms. The expiry reads back as a
    /// preset when the lifetime (`expiryMs - mintedAtMs`) is within
    /// ``presetToleranceMs`` of 1, 7 or 30 days, as **Never** only for `0`,
    /// else as Custom: whole days when the lifetime is whole days, otherwise
    /// hours (rounded, 1 at least). The uses read back as One (`0` or `1`),
    /// Up to 10, Unlimited (`UInt32.max`), else Custom with the number.
    static func prefilled(
        label: String, expiryMs: UInt64, maxUses: UInt32, mintedAtMs: UInt64
    ) -> InviteCreateForm {
        var form = InviteCreateForm()
        form.label = clampLabel(label)
        if expiryMs == 0 {
            form.expiry = .never
        } else {
            let lifetime = Int64(clamping: expiryMs) - Int64(clamping: mintedAtMs)
            let presets: [(Int64, Expiry)] = [(1, .oneDay), (7, .sevenDays), (30, .thirtyDays)]
            if let preset = presets.first(where: { abs(lifetime - $0.0 * dayMs) <= presetToleranceMs }) {
                form.expiry = preset.1
            } else {
                form.expiry = .custom
                let days = (Double(lifetime) / Double(dayMs)).rounded()
                if days >= 1, abs(lifetime - Int64(days) * dayMs) <= presetToleranceMs {
                    form.customExpiryUnit = .days
                    form.customExpiryAmount = String(Int64(days))
                } else {
                    form.customExpiryUnit = .hours
                    let hours = max(1, Int64((Double(lifetime) / Double(hourMs)).rounded()))
                    form.customExpiryAmount = String(hours)
                }
            }
        }
        switch maxUses {
        case 0, 1: form.uses = .one
        case 10: form.uses = .upToTen
        case unlimitedUsesWire: form.uses = .unlimited
        default:
            form.uses = .custom
            form.customUses = String(maxUses)
        }
        return form
    }

    /// `text` cut, at a character boundary, to ``labelMaxBytes`` UTF-8 bytes:
    /// the label field never holds more than the core accepts.
    static func clampLabel(_ text: String) -> String {
        guard text.utf8.count > labelMaxBytes else { return text }
        var out = ""
        var bytes = 0
        for character in text {
            let size = String(character).utf8.count
            if bytes + size > labelMaxBytes { break }
            out.append(character)
            bytes += size
        }
        return out
    }

    /// 1 to 9 decimal digits, surrounding spaces allowed; anything else is
    /// not a whole number (the web `parseWholeNumber`). The number pad of an
    /// Arabic or Persian interface types its own digits, so any script's
    /// decimal digits count.
    private static func parseWholeNumber(_ text: String) -> Int64? {
        let trimmed = text.trimmingCharacters(in: .whitespaces)
        guard (1...9).contains(trimmed.unicodeScalars.count) else { return nil }
        var value: Int64 = 0
        for scalar in trimmed.unicodeScalars {
            guard scalar.properties.numericType == .decimal,
                  let digit = scalar.properties.numericValue else { return nil }
            value = value * 10 + Int64(digit)
        }
        return value
    }
}
