// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import SwiftUI

/// The first screen on a device without a session (UX spec section 5.6):
/// the brand mark, the name, one line, and the two ways in. Everything
/// else in the anonymous flow is pushed from here.
struct WelcomeScreen: View {
    let onCreateAccount: () -> Void
    let onLogIn: () -> Void

    var body: some View {
        GeometryReader { proxy in
            ScrollView {
                VStack(spacing: 0) {
                    Spacer(minLength: 24)

                    VStack(spacing: 20) {
                        LakkoBrandMark(size: 96)
                        VStack(spacing: 8) {
                            Text("Lakko")
                                .font(.system(size: 28, weight: .bold))
                                .foregroundStyle(Color.lakkoText)
                            Text("Messages that stay between you.")
                                .font(.body)
                                .foregroundStyle(Color.lakkoTextMuted)
                                .multilineTextAlignment(.center)
                        }
                    }
                    .frame(maxWidth: .infinity)

                    Spacer(minLength: 40)

                    VStack(spacing: 12) {
                        AuthPrimaryButton(title: "Create account", action: onCreateAccount)
                        AuthSecondaryButton(title: "Log in", action: onLogIn)
                    }
                    .frame(maxWidth: 480)
                    .frame(maxWidth: .infinity)
                }
                .padding(.horizontal, 16)
                .padding(.vertical, 24)
                .frame(minHeight: proxy.size.height)
            }
        }
        .background(Color.lakkoBackground.ignoresSafeArea())
        .toolbar(.hidden, for: .navigationBar)
    }
}

/// The logo tile: the brand mark itself (the same PNG the web client and
/// Android ship, `LakkoBrandMark` in the asset catalogue), never a glyph
/// drawn in code, so the three clients show one logo.
struct LakkoBrandMark: View {
    var size: CGFloat = 96

    var body: some View {
        Image("LakkoBrandMark")
            .resizable()
            .interpolation(.high)
            .scaledToFit()
            .frame(width: size, height: size)
            .accessibilityLabel("Lakko")
    }
}
