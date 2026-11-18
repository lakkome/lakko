// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import SwiftUI
import UIKit

/// Settings > About > Support Lakko (`docs/ux/donations.md` section 4.1).
/// The screen reads the ways to donate through ``DonationsClient`` and shows
/// only what verified against the pinned donations key.
@MainActor
final class DonationsModel: ObservableObject {
    enum Phase: Equatable {
        case loading
        case loaded([DonateMethod])
        /// Nothing verified, nothing sent, or no pinned key.
        case none
        case error
    }

    @Published private(set) var phase: Phase = .loading

    let client: DonationsClient

    init(client: DonationsClient = DonationsClient()) {
        self.client = client
    }

    func load() async {
        phase = .loading
        do {
            let methods = try await client.fetchMethods()
            phase = methods.isEmpty ? .none : .loaded(methods)
        } catch {
            phase = .error
        }
    }
}

struct DonationsScreen: View {
    @StateObject private var model: DonationsModel
    @ObservedObject private var l10n = Localization.shared
    @State private var started = false

    init() {
        _model = StateObject(wrappedValue: DonationsModel())
    }

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                Text(l10n.t("donate.lede"))
                    .font(.body)
                    .foregroundStyle(Color.lakkoText)
                    .fixedSize(horizontal: false, vertical: true)

                switch model.phase {
                case .loading:
                    placeholder
                    placeholder
                case .error:
                    Text(l10n.t("donate.error"))
                        .foregroundStyle(Color.lakkoDanger)
                        .fixedSize(horizontal: false, vertical: true)
                    Button(l10n.t("common.try_again")) {
                        Task { await model.load() }
                    }
                    .buttonStyle(.borderedProminent)
                case .none:
                    Text(l10n.t("donate.none"))
                        .foregroundStyle(Color.lakkoTextMuted)
                        .fixedSize(horizontal: false, vertical: true)
                case .loaded(let methods):
                    ForEach(methods) { method in
                        if let crypto = method.crypto {
                            CryptoDonationCard(crypto: crypto)
                        } else {
                            PaymentDonationCard(method: method, client: model.client)
                        }
                    }
                }
            }
            .frame(maxWidth: 720, alignment: .leading)
            .padding(16)
            .frame(maxWidth: .infinity)
        }
        .background(Color.lakkoBackground)
        .navigationTitle(l10n.t("donate.title"))
        .navigationBarTitleDisplayMode(.inline)
        .task {
            guard !started else { return }
            started = true
            await model.load()
        }
    }

    private var placeholder: some View {
        RoundedRectangle(cornerRadius: LakkoRadius.card)
            .fill(Color.lakkoSurfaceSunken)
            .frame(height: 140)
            .accessibilityHidden(true)
    }
}

/// The elevated card the donation methods sit on (design system "Card").
private struct DonationCard<Content: View>: View {
    @ViewBuilder let content: Content

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            content
        }
        .padding(20)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Color.lakkoSurface)
        .clipShape(RoundedRectangle(cornerRadius: LakkoRadius.card))
        .overlay(
            RoundedRectangle(cornerRadius: LakkoRadius.card)
                .stroke(Color.lakkoBorder, lineWidth: 1)
        )
    }
}

private struct CardTitle: View {
    let title: String
    let subtitle: String

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(title)
                .font(.headline)
                .foregroundStyle(Color.lakkoText)
                .accessibilityAddTraits(.isHeader)
            Text(subtitle)
                .font(.footnote)
                .foregroundStyle(Color.lakkoTextMuted)
                .fixedSize(horizontal: false, vertical: true)
        }
    }
}

/// A button whose label reads "Copied" for two seconds after it copied.
private struct CopyButton: View {
    let text: String
    let label: String
    @ObservedObject private var l10n = Localization.shared
    @State private var copied = false

    var body: some View {
        Button(copied ? l10n.t("donate.copied") : label) {
            UIPasteboard.general.string = text
            copied = true
            Task {
                try? await Task.sleep(nanoseconds: 2_000_000_000)
                copied = false
            }
        }
        .buttonStyle(.bordered)
    }
}

private struct PaymentDonationCard: View {
    let method: DonateMethod
    let client: DonationsClient
    @ObservedObject private var l10n = Localization.shared
    @Environment(\.openURL) private var openURL

    private var name: String {
        switch method.kind {
        case .sbp: return "SBP"
        case .revolut: return "Revolut"
        case .bmc: return "Buy Me a Coffee"
        case .crypto: return ""
        }
    }

    var body: some View {
        DonationCard {
            CardTitle(
                title: name,
                subtitle: method.kind == .sbp ? l10n.t("donate.for_russia") : l10n.t("donate.worldwide"))
            if let sha = method.qrSha256 {
                VerifiedQrView(id: method.id, sha256: sha, client: client)
            }
            if let link = method.link {
                Button(l10n.t("donate.button")) {
                    // The core admits `https://` only; checked again here.
                    guard link.scheme == "https" else { return }
                    openURL(link)
                }
                .buttonStyle(.borderedProminent)
            }
            if method.hasPhone {
                PhoneRow(id: method.id, client: client)
            }
        }
    }
}

/// A method's QR image, shown only once its bytes hash to the signed SHA-256;
/// on a mismatch or a failure nothing is shown.
private struct VerifiedQrView: View {
    let id: String
    let sha256: Data
    let client: DonationsClient
    @ObservedObject private var l10n = Localization.shared
    @State private var image: UIImage?

    var body: some View {
        Group {
            if let image {
                Image(uiImage: image)
                    .resizable()
                    .interpolation(.none)
                    .scaledToFit()
                    .frame(width: 200, height: 200)
                    .accessibilityLabel(l10n.t("common.qr_code"))
                    .accessibilityAddTraits(.isImage)
            }
        }
        .task(id: id) {
            guard image == nil,
                  let data = await client.fetchQr(id: id, expectedSha256: sha256),
                  let decoded = UIImage(data: data)
            else { return }
            image = decoded
        }
    }
}

/// The masked phone number, `Show`, then the number and `Copy`. The number
/// lives in this view's state only.
private struct PhoneRow: View {
    let id: String
    let client: DonationsClient
    @ObservedObject private var l10n = Localization.shared
    @State private var phone: String?
    @State private var busy = false
    @State private var failed = false

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 12) {
                Text(phone ?? DonationsClient.maskedPhone)
                    .font(.body.monospaced())
                    .foregroundStyle(Color.lakkoText)
                    .textSelection(.enabled)
                    // The dots of the mask are not read out; VoiceOver
                    // says the number is hidden and, once shown, reads it.
                    .accessibilityLabel(phone ?? l10n.t("donate.phone_hidden"))
                Spacer(minLength: 0)
                if let phone {
                    CopyButton(text: phone, label: l10n.t("donate.copy"))
                } else {
                    Button(l10n.t("donate.phone_show")) {
                        Task { await reveal() }
                    }
                    .buttonStyle(.bordered)
                    .disabled(busy)
                }
            }
            if failed {
                Text(l10n.t("donate.phone_error"))
                    .font(.footnote)
                    .foregroundStyle(Color.lakkoDanger)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
    }

    private func reveal() async {
        busy = true
        failed = false
        defer { busy = false }
        do {
            phone = try await client.revealPhone(id: id)
        } catch {
            failed = true
        }
    }
}

private struct CryptoDonationCard: View {
    let crypto: DonateMethod.Crypto
    @ObservedObject private var l10n = Localization.shared

    var body: some View {
        DonationCard {
            CardTitle(
                title: crypto.coin,
                subtitle: l10n.t("donate.crypto_network", texts: ["network": crypto.network]))
            HStack {
                Spacer(minLength: 0)
                BrandedQrImage(text: crypto.address, size: 240)
                Spacer(minLength: 0)
            }
            Text(crypto.address)
                .font(.body.monospaced())
                .foregroundStyle(Color.lakkoText)
                .textSelection(.enabled)
                .fixedSize(horizontal: false, vertical: true)
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(10)
                .background(Color.lakkoSurfaceSunken)
                .clipShape(RoundedRectangle(cornerRadius: LakkoRadius.control))
            CopyButton(text: crypto.address, label: l10n.t("donate.crypto_copy"))
            Text(l10n.t("donate.crypto_warning", texts: ["coin": crypto.coin, "network": crypto.network]))
                .font(.footnote)
                .foregroundStyle(Color.lakkoWarning)
                .fixedSize(horizontal: false, vertical: true)
        }
    }
}
