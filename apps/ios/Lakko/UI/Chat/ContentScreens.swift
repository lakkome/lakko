// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import CoreLocation
import LakkoCore
import SwiftUI

// ADR-0090 / `docs/ux/attachments.md` — the location screen (section 9) and
// the contact picker (section 10).

// MARK: - Location

/// One fix kept updating while the location screen is open (UX section 9.2).
/// Nothing leaves the device before `Send`.
@MainActor
final class LocationFix: NSObject, ObservableObject, CLLocationManagerDelegate {
    enum Phase: Equatable {
        case asking
        case denied
        case finding
        case fixed
        case notFound
    }

    @Published private(set) var phase: Phase = .asking
    @Published private(set) var location: CLLocation?
    @Published private(set) var approximate = false

    private let manager = CLLocationManager()
    private var timeout: Task<Void, Never>?
    /// UX section 9.4 — no fix after 30 s (chosen).
    static let noFixAfterSeconds: UInt64 = 30

    override init() {
        super.init()
        manager.delegate = self
        manager.desiredAccuracy = kCLLocationAccuracyBest
    }

    func start() {
        switch manager.authorizationStatus {
        case .notDetermined:
            phase = .asking
            manager.requestWhenInUseAuthorization()
        case .denied, .restricted:
            phase = .denied
        default:
            begin()
        }
    }

    func stop() {
        manager.stopUpdatingLocation()
        timeout?.cancel()
    }

    func retry() { begin() }

    private func begin() {
        approximate = manager.accuracyAuthorization == .reducedAccuracy
        phase = location == nil ? .finding : .fixed
        manager.startUpdatingLocation()
        timeout?.cancel()
        timeout = Task { @MainActor [weak self] in
            try? await Task.sleep(nanoseconds: Self.noFixAfterSeconds * 1_000_000_000)
            guard let self, !Task.isCancelled, self.location == nil else { return }
            self.phase = .notFound
            self.manager.stopUpdatingLocation()
        }
    }

    nonisolated func locationManagerDidChangeAuthorization(_ manager: CLLocationManager) {
        Task { @MainActor in
            switch manager.authorizationStatus {
            case .denied, .restricted: self.phase = .denied
            case .authorizedWhenInUse, .authorizedAlways: self.begin()
            default: break
            }
        }
    }

    nonisolated func locationManager(_ manager: CLLocationManager, didUpdateLocations locations: [CLLocation]) {
        guard let last = locations.last else { return }
        Task { @MainActor in
            self.location = last
            self.phase = .fixed
        }
    }

    nonisolated func locationManager(_ manager: CLLocationManager, didFailWithError error: Error) {}

    /// The fix as sent (1e-7 degree fixed point, ADR-0090 section 6).
    func shared(name: String) -> SharedLocation? {
        guard let location else { return nil }
        let limits = contentLimits()
        let trimmed = ContentText.clamp(ContentText.trimmed(name), bytes: limits.maxLocationNameBytes)
        return SharedLocation(
            latE7: Int32(clamping: Int64((location.coordinate.latitude * 1e7).rounded())),
            lonE7: Int32(clamping: Int64((location.coordinate.longitude * 1e7).rounded())),
            accuracyM: UInt32(clamping: max(1, Int(location.horizontalAccuracy.rounded()))),
            fixAtMs: Int64(location.timestamp.timeIntervalSince1970 * 1000),
            name: trimmed.isEmpty ? nil : trimmed)
    }
}

/// Formatting of a location (UX section 9.2): coordinates in the person's
/// locale, the accuracy as a platform-formatted distance.
enum LocationText {
    static func coordinates(latE7: Int32, lonE7: Int32) -> String {
        let f = NumberFormatter()
        f.minimumFractionDigits = 5
        f.maximumFractionDigits = 5
        let lat = Double(latE7) / 1e7, lon = Double(lonE7) / 1e7
        let latText = f.string(from: NSNumber(value: abs(lat))) ?? "\(abs(lat))"
        let lonText = f.string(from: NSNumber(value: abs(lon))) ?? "\(abs(lon))"
        return "\(latText)° \(lat >= 0 ? "N" : "S"), \(lonText)° \(lon >= 0 ? "E" : "W")"
    }

    static func distance(meters: UInt32) -> String {
        let f = MeasurementFormatter()
        f.unitOptions = .naturalScale
        f.numberFormatter.maximumFractionDigits = meters >= 1000 ? 1 : 0
        return f.string(from: Measurement(value: Double(meters), unit: UnitLength.meters))
    }

    /// UX section 9.4 — warning colour when older than 2 minutes or worse
    /// than 1 km (both chosen).
    static func isWeak(accuracyM: UInt32, fixAtMs: Int64, nowMs: Int64) -> Bool {
        accuracyM > 1000 || nowMs - fixAtMs > 120_000
    }

    /// The Apple Maps URL a tap opens (UX section 9.3).
    static func appleMapsURL(latE7: Int32, lonE7: Int32, name: String?) -> URL? {
        var c = URLComponents(string: "https://maps.apple.com/")
        let ll = "\(Double(latE7) / 1e7),\(Double(lonE7) / 1e7)"
        c?.queryItems = [URLQueryItem(name: "ll", value: ll), URLQueryItem(name: "q", value: name ?? ll)]
        return c?.url
    }
}

/// UX section 9 — Lakko's location screen.
struct LocationScreen: View {
    let onSend: (SharedLocation) -> Void
    let onCancel: () -> Void

    @StateObject private var fix = LocationFix()
    @ObservedObject private var l10n = Localization.shared
    @State private var name = ""
    @Environment(\.openURL) private var openURL

    var body: some View {
        NavigationStack {
            VStack(spacing: 20) {
                Spacer()
                Image(systemName: "mappin.circle.fill")
                    .font(.system(size: 72))
                    .foregroundStyle(Color.lakkoAccent)
                    .accessibilityHidden(true)
                content
                Spacer()
            }
            .padding(.horizontal, 24)
            .navigationTitle(l10n.t("location.title"))
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button(l10n.t("common.cancel")) { onCancel() }
                }
            }
        }
        .onAppear { fix.start() }
        .onDisappear { fix.stop() }
    }

    @ViewBuilder
    private var content: some View {
        switch fix.phase {
        case .denied:
            Text(l10n.t("location.denied")).multilineTextAlignment(.center)
            Button(l10n.t("chat.record.open_settings")) {
                if let url = URL(string: UIApplication.openSettingsURLString) { openURL(url) }
            }
        case .notFound:
            Text(l10n.t("location.not_found")).multilineTextAlignment(.center)
            Button(l10n.t("common.try_again")) { fix.retry() }
        case .asking, .finding:
            ProgressView()
            Text(l10n.t("location.finding")).foregroundStyle(Color.lakkoTextMuted)
            sendButton(enabled: false)
        case .fixed:
            if let shared = fix.shared(name: name) {
                Text(LocationText.coordinates(latE7: shared.latE7, lonE7: shared.lonE7))
                    .font(.title3.monospacedDigit())
                Text(fix.approximate
                     ? l10n.t("location.approximate")
                     : l10n.t("location.accuracy", texts: ["distance": LocationText.distance(meters: shared.accuracyM)]))
                    .font(.footnote)
                    .foregroundStyle(LocationText.isWeak(
                        accuracyM: shared.accuracyM, fixAtMs: shared.fixAtMs, nowMs: ChatRepository.nowMs())
                        ? Color.lakkoWarning : Color.lakkoTextMuted)
            }
            TextField(l10n.t("location.name_place"), text: Binding(
                get: { name }, set: { name = ContentText.clamp($0, bytes: contentLimits().maxLocationNameBytes) }))
                .textFieldStyle(.roundedBorder)
            sendButton(enabled: true)
        }
    }

    private func sendButton(enabled: Bool) -> some View {
        Button {
            if let shared = fix.shared(name: name) { onSend(shared) }
        } label: {
            Text(l10n.t("location.send"))
                .frame(maxWidth: .infinity, minHeight: 44)
        }
        .buttonStyle(.borderedProminent)
        .tint(Color.lakkoAccent)
        .disabled(!enabled)
    }
}

// MARK: - Contact picker

/// One row of the contact picker.
struct ContactCardCandidate: Identifiable, Equatable {
    let id: String
    let name: String
    let username: String?
    let accountKey: Data
    /// ADR-0090 section 7 — the newest profile held from this contact lets
    /// others share their card (`true` when none is held).
    var shareable: Bool = true

    /// Whether the row can be picked, and if not why (UX section 10.1).
    enum Availability: Equatable {
        case available
        /// Their "who can find me" is `Nobody`: their card is never sent.
        case notShareable
        /// No username to put on the card.
        case noUsername
    }

    var availability: Availability {
        if !shareable { return .notShareable }
        if username == nil { return .noUsername }
        return .available
    }

    /// One row for the contact holding `accountKey`, read from the newest
    /// profile this device holds from them.
    @MainActor
    static func make(accountKey: Data, name: String) -> ContactCardCandidate {
        let keyB64 = accountKey.base64EncodedString()
        let profile = ProfileDirectory.shared.storedProfile(signatureKeyB64: keyB64)
        return ContactCardCandidate(
            id: keyB64, name: name,
            username: profile?.username.flatMap { $0.isEmpty ? nil : $0 },
            accountKey: accountKey,
            shareable: profile?.cardShareable ?? true)
    }
}

/// UX section 10.1 — the contact picker (single choice), then the card
/// preview and `Send`.
struct ContactCardPickerScreen: View {
    let candidates: [ContactCardCandidate]
    let onSend: (SharedContactCard) -> Void
    let onCancel: () -> Void

    @ObservedObject private var l10n = Localization.shared
    @State private var query = ""
    @State private var picked: ContactCardCandidate?

    private var shown: [ContactCardCandidate] {
        let q = ContentText.trimmed(query).lowercased()
        guard !q.isEmpty else { return candidates }
        return candidates.filter {
            $0.name.lowercased().contains(q) || ($0.username?.contains(q) ?? false)
        }
    }

    var body: some View {
        NavigationStack {
            Group {
                if let picked {
                    VStack(spacing: 24) {
                        ContactCardView(
                            name: picked.name, username: picked.username ?? "", action: nil)
                        Button {
                            guard picked.availability == .available, let username = picked.username else { return }
                            onSend(SharedContactCard(name: picked.name, username: username, accountKey: picked.accountKey))
                        } label: {
                            Text(l10n.t("attach.preview.send")).frame(maxWidth: .infinity, minHeight: 44)
                        }
                        .buttonStyle(.borderedProminent)
                        .tint(Color.lakkoAccent)
                        Spacer()
                    }
                    .padding(24)
                } else {
                    List(shown) { candidate in
                        let available = candidate.availability == .available
                        Button { picked = candidate } label: {
                            VStack(alignment: .leading, spacing: 2) {
                                Text(candidate.name).foregroundStyle(Color.lakkoText)
                                Text(subtitle(candidate))
                                    .font(.footnote)
                                    .foregroundStyle(Color.lakkoTextMuted)
                            }
                        }
                        .disabled(!available)
                        .opacity(available ? 1 : 0.5)
                    }
                    .searchable(text: $query)
                }
            }
            .navigationTitle(l10n.t("attach.menu.contact"))
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button(l10n.t("common.cancel")) {
                        if picked != nil { picked = nil } else { onCancel() }
                    }
                }
            }
        }
    }

    private func subtitle(_ candidate: ContactCardCandidate) -> String {
        switch candidate.availability {
        case .available: return "@" + (candidate.username ?? "")
        case .notShareable: return l10n.t("contact.not_shareable", texts: ["name": candidate.name])
        case .noUsername: return l10n.t("contact.no_username")
        }
    }
}

/// The contacts a card can be made of (UX section 10.1): every accepted
/// contact, with the username its own profile carries (none = disabled).
enum ContactCardCandidates {
    @MainActor
    static func load(nameOf: (String) -> String? = { _ in nil }) -> [ContactCardCandidate] {
        AcceptedContactsStore.load().map { c in
            ContactCardCandidate.make(
                accountKey: c.request.fromPubkey,
                name: nameOf(c.request.fromPubkey.base64EncodedString()) ?? c.displayLabel)
        }
        .sorted { $0.name.localizedCaseInsensitiveCompare($1.name) == .orderedAscending }
    }
}
