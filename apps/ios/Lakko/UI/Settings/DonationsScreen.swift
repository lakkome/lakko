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
    /// The verified QR images of the payment methods, by method id. A method
    /// is absent while its image loads and when it did not verify or decode.
    @Published private(set) var qrImages: [String: UIImage] = [:]

    let client: DonationsClient

    init(client: DonationsClient = DonationsClient()) {
        self.client = client
    }

    /// Load the list, show the cards, then fill in each payment QR once its
    /// bytes hash to the signed SHA-256 and decode as an image. The QR images
    /// are loaded here and not by the card's own view: a view that shows
    /// nothing until its image arrives has no body to run a task from, which
    /// is how the first build never asked for one.
    func load() async {
        phase = .loading
        qrImages = [:]
        let methods: [DonateMethod]
        do {
            methods = try await client.fetchMethods()
        } catch {
            phase = .error
            return
        }
        phase = methods.isEmpty ? .none : .loaded(methods)
        await loadQrImages(methods)
    }

    private func loadQrImages(_ methods: [DonateMethod]) async {
        let client = client
        await withTaskGroup(of: (String, Data?).self) { group in
            for method in methods {
                guard let sha = method.qrSha256 else { continue }
                let id = method.id
                group.addTask { (id, await client.fetchQr(id: id, expectedSha256: sha)) }
            }
            for await (id, data) in group {
                if let data, let image = UIImage(data: data) {
                    qrImages[id] = image
                }
            }
        }
    }
}

struct DonationsScreen: View {
    @StateObject private var model: DonationsModel
    @ObservedObject private var l10n = Localization.shared
    @State private var started = false

    @MainActor
    init() {
        _model = StateObject(wrappedValue: DonationsModel())
    }

    /// A screen over a model the caller made (tests).
    init(model: DonationsModel) {
        _model = StateObject(wrappedValue: model)
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
                    let groups = DonationsClient.groups(methods)
                    if !groups.payment.isEmpty {
                        SectionHeading(text: l10n.t("donate.section_payment"))
                        ForEach(groups.payment) { method in
                            PaymentDonationCard(
                                method: method, qrImage: model.qrImages[method.id], client: model.client)
                        }
                    }
                    if !groups.crypto.isEmpty {
                        SectionHeading(text: l10n.t("donate.section_crypto"))
                        ForEach(groups.crypto) { method in
                            if let crypto = method.crypto {
                                CryptoDonationCard(crypto: crypto)
                            }
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

/// The small heading over a group of cards ("Bank and card", "Crypto").
private struct SectionHeading: View {
    let text: String

    var body: some View {
        Text(text)
            .font(.subheadline.weight(.semibold))
            .foregroundStyle(Color.lakkoTextMuted)
            .accessibilityAddTraits(.isHeader)
            .padding(.top, 8)
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

/// The card's full-width primary action (Donate, Copy address, Copy).
private struct CardActionButton: View {
    let title: String
    var prominent = true
    let action: () -> Void

    var body: some View {
        let button = Button(action: action) {
            Text(title)
                .font(.body.weight(.semibold))
                .frame(maxWidth: .infinity)
        }
        .controlSize(.large)
        if prominent {
            button.buttonStyle(.borderedProminent)
        } else {
            button.buttonStyle(.bordered)
        }
    }
}

/// A full-width primary button whose label reads "Copied" for two seconds
/// after it copied.
private struct CopyButton: View {
    let text: String
    let label: String
    @ObservedObject private var l10n = Localization.shared
    @State private var copied = false

    var body: some View {
        CardActionButton(title: copied ? l10n.t("donate.copied") : label) {
            UIPasteboard.general.string = text
            copied = true
            Task {
                try? await Task.sleep(nanoseconds: 2_000_000_000)
                copied = false
            }
        }
    }
}

private struct PaymentDonationCard: View {
    let method: DonateMethod
    let qrImage: UIImage?
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
            if let qrImage {
                PaymentQrImage(image: qrImage)
            }
            if let link = method.link {
                CardActionButton(title: l10n.t("donate.button")) {
                    // The core admits `https://` only; checked again here.
                    guard link.scheme == "https" else { return }
                    openURL(link)
                }
            }
            if method.hasPhone {
                PhoneRow(id: method.id, client: client)
            }
        }
    }
}

/// A payment method's verified QR image: the operator's PNG as served, in
/// colour, as wide as the card (the same size as a crypto code).
private struct PaymentQrImage: View {
    let image: UIImage
    @ObservedObject private var l10n = Localization.shared

    var body: some View {
        Image(uiImage: image)
            .resizable()
            .interpolation(.high)
            .scaledToFit()
            .frame(maxWidth: .infinity)
            .accessibilityLabel(l10n.t("common.qr_code"))
            .accessibilityAddTraits(.isImage)
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
        VStack(alignment: .leading, spacing: 8) {
            Text(phone ?? DonationsClient.maskedPhone)
                .font(.body.monospaced())
                .foregroundStyle(Color.lakkoText)
                .textSelection(.enabled)
                .frame(maxWidth: .infinity, alignment: .leading)
                // The dots of the mask are not read out; VoiceOver
                // says the number is hidden and, once shown, reads it.
                .accessibilityLabel(phone ?? l10n.t("donate.phone_hidden"))
            if let phone {
                CopyButton(text: phone, label: l10n.t("donate.copy"))
            } else {
                CardActionButton(title: l10n.t("donate.phone_show"), prominent: false) {
                    Task { await reveal() }
                }
                .disabled(busy)
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
            DonationQrView(address: crypto.address, icon: crypto.icon)
            Text(crypto.address)
                .font(.body.monospaced())
                .foregroundStyle(Color.lakkoText)
                .textSelection(.enabled)
                .fixedSize(horizontal: false, vertical: true)
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(10)
                .background(Color.lakkoSurfaceSunken)
                .clipShape(RoundedRectangle(cornerRadius: LakkoRadius.control))
            Text(l10n.t("donate.crypto_warning", texts: ["coin": crypto.coin, "network": crypto.network]))
                .font(.footnote)
                .foregroundStyle(Color.lakkoWarning)
                .fixedSize(horizontal: false, vertical: true)
            CopyButton(text: crypto.address, label: l10n.t("donate.crypto_copy"))
        }
    }
}
