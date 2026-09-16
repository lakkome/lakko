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

/// The logo tile: the brand gradient (design system section 1, the one
/// surface allowed to carry it) under a script "L". Used once, here; the
/// app icon carries the same mark.
struct LakkoBrandMark: View {
    var size: CGFloat = 96

    var body: some View {
        ZStack {
            RoundedRectangle(cornerRadius: size * 0.22, style: .continuous)
                .fill(LakkoBrand.gradient)
            Text("L")
                .font(.system(size: size * 0.6, weight: .bold, design: .serif))
                .italic()
                .foregroundStyle(.white)
                .offset(x: -size * 0.02, y: -size * 0.03)
        }
        .frame(width: size, height: size)
        .accessibilityLabel("Lakko")
    }
}
