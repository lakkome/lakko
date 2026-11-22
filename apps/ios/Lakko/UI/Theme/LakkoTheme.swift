// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import SwiftUI
import UIKit

// MARK: - Colour tokens

/// The one colour vocabulary every iOS screen draws from
/// (docs/ux/client-redesign.md section 4). Each name is a colour set in
/// `Lakko/Assets.xcassets` carrying a light and a dark variant, so a screen
/// never decides its own palette and dark mode follows the system (or the
/// override in Settings > Appearance) everywhere.
///
/// Calls opt into `.preferredColorScheme(.dark)` and read the same tokens,
/// which resolve to the dark half there; nothing else forces a scheme.
extension Color {
    /// Page / window background.
    static let lakkoBackground = Color("LakkoBackground")
    /// Cards, panes, the composer, dialogs.
    static let lakkoSurface = Color("LakkoSurface")
    /// Search fields, incoming bubbles, code.
    static let lakkoSurfaceSunken = Color("LakkoSurfaceSunken")
    /// Selected list row / nav item.
    static let lakkoSurfaceSelected = Color("LakkoSurfaceSelected")
    /// Hairlines, input borders.
    static let lakkoBorder = Color("LakkoBorder")
    /// Primary text.
    static let lakkoText = Color("LakkoText")
    /// Secondary text, timestamps.
    static let lakkoTextMuted = Color("LakkoTextMuted")
    /// Primary actions, links, the outgoing bubble.
    static let lakkoAccent = Color("LakkoAccent")
    /// Text on the accent.
    static let lakkoAccentForeground = Color("LakkoAccentForeground")
    /// Accent tint (unread badge ground, avatar tint).
    static let lakkoAccentSoft = Color("LakkoAccentSoft")
    /// Destructive actions, errors.
    static let lakkoDanger = Color("LakkoDanger")
    /// Delivered / connected.
    static let lakkoSuccess = Color("LakkoSuccess")
    /// Recoverable warnings (design system `--warning`; not in the section-4
    /// table, added because the call quality glyph needs a middle tier).
    static let lakkoWarning = Color("LakkoWarning")
}

// MARK: - Shape and spacing

/// Corner radii from the spec: controls 8, cards and rows 12, bubbles 18.
enum LakkoRadius {
    static let control: CGFloat = 8
    static let card: CGFloat = 12
    static let bubble: CGFloat = 18
}

// MARK: - Appearance override

/// Settings > Appearance: follow the system, or force light / dark.
/// Persisted in `UserDefaults` under ``storageKey`` and applied once at the
/// root with `.preferredColorScheme`, so every screen (and every sheet
/// presented from it) follows the choice.
enum AppearanceSetting: String, CaseIterable, Identifiable {
    case system
    case light
    case dark

    static let storageKey = "lakko.appearance"

    var id: String { rawValue }

    /// The catalog key of the option's label (ADR-0061).
    var labelKey: String {
        switch self {
        case .system: return "settings.appearance.theme.system"
        case .light: return "settings.appearance.theme.light"
        case .dark: return "settings.appearance.theme.dark"
        }
    }

    /// `nil` means "do not override" (follow the system).
    var colorScheme: ColorScheme? {
        switch self {
        case .system: return nil
        case .light: return .light
        case .dark: return .dark
        }
    }
}

// MARK: - Chat text size

/// `docs/ux/text-size.md` — Settings > Appearance > Text size: one step that
/// scales message text and reactions only, on top of Dynamic Type. Kept in
/// the standard `UserDefaults` under ``storageKey``: it belongs to this
/// device, never the App Group, never another device or a server.
enum ChatTextSize: String, CaseIterable, Identifiable {
    case small
    case medium
    case large

    static let storageKey = "lakko.chat-text-size"

    /// Medium is the design system as it is.
    static let defaultStep: ChatTextSize = .medium

    var id: String { rawValue }

    /// The multiplier on every scaled size (chosen, not measured).
    var factor: CGFloat {
        switch self {
        case .small: return 0.87
        case .medium: return 1.0
        case .large: return 1.2
        }
    }

    /// The catalog key of the step's name (ADR-0061).
    var labelKey: String { "settings.text_size.\(rawValue)" }

    /// A stored value this build does not recognise reads as Medium.
    static func from(stored raw: String?) -> ChatTextSize {
        raw.flatMap(ChatTextSize.init(rawValue:)) ?? defaultStep
    }

    static func read(_ defaults: UserDefaults = .standard) -> ChatTextSize {
        from(stored: defaults.string(forKey: storageKey))
    }

    static func write(_ step: ChatTextSize, _ defaults: UserDefaults = .standard) {
        defaults.set(step.rawValue, forKey: storageKey)
    }
}

private struct ChatTextScaleKey: EnvironmentKey {
    static let defaultValue: CGFloat = 1.0
}

extension EnvironmentValues {
    /// The ``ChatTextSize`` factor for the message text and reactions below.
    var chatTextScale: CGFloat {
        get { self[ChatTextScaleKey.self] }
        set { self[ChatTextScaleKey.self] = newValue }
    }
}

/// Applied once at the root: reads the stored step and hands its factor to
/// everything below, so a change applies at once, everywhere.
private struct ChatTextScaleRoot: ViewModifier {
    @AppStorage(ChatTextSize.storageKey) private var raw = ChatTextSize.defaultStep.rawValue

    func body(content: Content) -> some View {
        content.environment(\.chatTextScale, ChatTextSize.from(stored: raw).factor)
    }
}

/// A text style's Dynamic Type size, multiplied by the chat text size. The
/// person's system text size still applies; the step multiplies it.
private struct ChatScaledFont: ViewModifier {
    let style: UIFont.TextStyle
    /// A fixed design size scaled like `style` (for a `.system(size:)` font).
    var base: CGFloat?
    var weight: Font.Weight?
    var italic = false
    var monospacedDigit = false

    @Environment(\.chatTextScale) private var scale
    @Environment(\.dynamicTypeSize) private var dynamicTypeSize

    func body(content: Content) -> some View {
        let font = Font.system(size: Self.size(style: style, base: base, dynamicTypeSize: dynamicTypeSize) * scale,
                               weight: weight ?? .regular)
        let styled = italic ? font.italic() : font
        return content.font(monospacedDigit ? styled.monospacedDigit() : styled)
    }

    static func size(style: UIFont.TextStyle, base: CGFloat?, dynamicTypeSize: DynamicTypeSize) -> CGFloat {
        let traits = UITraitCollection(preferredContentSizeCategory: UIContentSizeCategory(dynamicTypeSize))
        if let base {
            return UIFontMetrics(forTextStyle: style).scaledValue(for: base, compatibleWith: traits)
        }
        return UIFont.preferredFont(forTextStyle: style, compatibleWith: traits).pointSize
    }
}

/// The point size ``View/chatFont(_:weight:italic:monospacedDigit:)`` gives
/// a text style, for UIKit text (the composer's text view).
enum ChatFontSize {
    static func points(style: UIFont.TextStyle, dynamicTypeSize: DynamicTypeSize, scale: CGFloat) -> CGFloat {
        ChatScaledFont.size(style: style, base: nil, dynamicTypeSize: dynamicTypeSize) * scale
    }
}

extension View {
    /// Read the stored chat text size at the root of a window.
    func chatTextScaleRoot() -> some View { modifier(ChatTextScaleRoot()) }

    /// `docs/ux/text-size.md` — a message-text font: `style`'s Dynamic Type
    /// size times the chat text size.
    func chatFont(
        _ style: UIFont.TextStyle, weight: Font.Weight? = nil, italic: Bool = false,
        monospacedDigit: Bool = false
    ) -> some View {
        modifier(ChatScaledFont(style: style, weight: weight, italic: italic, monospacedDigit: monospacedDigit))
    }

    /// The same for a fixed design size, scaled with Dynamic Type like `style`.
    func chatFont(size: CGFloat, relativeTo style: UIFont.TextStyle, weight: Font.Weight? = nil) -> some View {
        modifier(ChatScaledFont(style: style, base: size, weight: weight))
    }
}

// MARK: - Touch targets

/// The smallest touch area of an action (Apple Human Interface Guidelines).
let lakkoMinTouchTarget: CGFloat = 44

extension View {
    /// A text action keeps its look and takes taps in at least a
    /// ``lakkoMinTouchTarget`` area centred on it. Put it on a plain or
    /// borderless button's LABEL: those styles hit-test the label alone, so a
    /// one-line `Retry` or `Remove` takes taps only on its glyphs. Layout is
    /// unchanged; the larger area is a content shape around the label.
    func lakkoTouchTarget() -> some View { modifier(LakkoTouchTarget()) }
}

private struct LakkoTouchTarget: ViewModifier {
    @State private var size: CGSize = .zero

    func body(content: Content) -> some View {
        let dx = max(0, (lakkoMinTouchTarget - size.width) / 2)
        let dy = max(0, (lakkoMinTouchTarget - size.height) / 2)
        content
            .onGeometryChange(for: CGSize.self) { $0.size } action: { size = $0 }
            .padding(.horizontal, dx)
            .padding(.vertical, dy)
            .contentShape(Rectangle())
            .padding(.horizontal, -dx)
            .padding(.vertical, -dy)
    }
}

// MARK: - Initials avatar

/// A circle with one or two initials on one of four tints chosen by a hash of
/// the identity, the iOS form of the web `Avatar` rule (design system
/// section 2). The identity string is only hashed, never shown.
struct InitialsAvatar: View {
    let name: String
    /// What the tint is derived from: an account id, a signature key, a
    /// group id. Falls back to the name when `nil`.
    var identity: String? = nil
    var size: CGFloat = 40
    /// `true` renders the group glyph instead of initials.
    var isGroup: Bool = false
    /// An icon drawn when the name yields no initials (a person nothing is
    /// known about), e.g. `.personFill`. `nil` leaves the circle blank.
    var placeholderIcon: LkIcon? = nil
    /// ADR-0084 — whose profile photo to show, when this device holds it.
    /// `nil` (a group, a channel, someone with no key) draws initials only.
    var photo: AvatarPhoto? = nil
    /// Play a profile video muted and looped (UX profile-screen section 7:
    /// large views and the chat header always, Chats and Contacts list rows
    /// while a pointer hovers them); otherwise the still is shown. The video
    /// is downloaded the first time it is asked for; the still stays until it
    /// is ready.
    var playsVideo: Bool = false

    var body: some View {
        if isDeletedAccount {
            deletedCircle
        } else if let photo {
            AvatarPhotoView(photo: photo, size: size, playsVideo: playsVideo) { initialsCircle }
        } else {
            initialsCircle
        }
    }

    /// ADR-0098 — a person drawn under ``ResolvedDisplayName/deletedName``:
    /// the neutral avatar, the person glyph on the neutral surface, no photo
    /// and no initials, the same on every client.
    private var isDeletedAccount: Bool {
        !isGroup && name == ResolvedDisplayName.deletedName
    }

    private var deletedCircle: some View {
        ZStack {
            Circle().fill(Color.lakkoSurfaceSunken)
            LkIconView(.personFill, size: size * 0.5, relativeTo: nil)
                .foregroundStyle(Color.lakkoTextMuted)
        }
        .frame(width: size, height: size)
        .accessibilityHidden(true)
    }

    private var initialsCircle: some View {
        let tint = Self.tint(for: identity ?? name)
        return ZStack {
            Circle().fill(tint.background)
            if isGroup && initials.isEmpty {
                LkIconView(.person3Fill, size: size * 0.45, relativeTo: nil)
                    .foregroundStyle(tint.foreground)
            } else if initials.isEmpty, let placeholderIcon {
                LkIconView(placeholderIcon, size: size * 0.5, relativeTo: nil)
                    .foregroundStyle(tint.foreground)
            } else {
                Text(initials)
                    .font(.system(size: size * 0.4, weight: .semibold, design: .rounded))
                    .foregroundStyle(tint.foreground)
            }
        }
        .frame(width: size, height: size)
        .accessibilityHidden(true)
    }

    /// The shared initials rule (`lakko_ui_rules::avatar_initials`, the
    /// design-system Avatar bullet): written once in Rust so the four clients
    /// cannot disagree. Empty for a name with nothing to show; the circle then
    /// shows its placeholder.
    var initials: String {
        avatarInitials(name: name)
    }

    /// Four tints (indigo, teal, slate, sand), light and dark, per the design
    /// system; the pick is a stable hash of the identity string.
    struct Tint {
        let background: Color
        let foreground: Color
    }

    static func tint(for identity: String) -> Tint {
        var hash: UInt32 = 2_166_136_261
        for byte in identity.utf8 {
            hash ^= UInt32(byte)
            hash = hash &* 16_777_619
        }
        switch hash % 4 {
        case 0:
            return Tint(background: .lakkoAccentSoft, foreground: .lakkoAccent)
        case 1:
            return Tint(
                background: Color(light: 0xE0F2F6, dark: 0x12313A),
                foreground: Color(light: 0x0F6F80, dark: 0x6FD0E0))
        case 2:
            return Tint(
                background: Color(light: 0xE9EBF3, dark: 0x252A3D),
                foreground: Color(light: 0x46506E, dark: 0xAEB6D0))
        default:
            return Tint(
                background: Color(light: 0xF6EFE3, dark: 0x3A2F17),
                foreground: Color(light: 0x8A5A17, dark: 0xE5B96B))
        }
    }
}

extension Color {
    /// A colour with one value per scheme, for the avatar tints that have no
    /// asset of their own. Everything else goes through the catalog.
    init(light: UInt32, dark: UInt32) {
        self.init(uiColor: UIColor { traits in
            let value = traits.userInterfaceStyle == .dark ? dark : light
            return UIColor(
                red: CGFloat((value >> 16) & 0xFF) / 255,
                green: CGFloat((value >> 8) & 0xFF) / 255,
                blue: CGFloat(value & 0xFF) / 255,
                alpha: 1)
        })
    }
}

// MARK: - Brand

/// The brand gradient (design system section 1): dark indigo to light
/// indigo, the same in both schemes. It appears on the logo tile only,
/// never on a page, card, button or bubble.
enum LakkoBrand {
    static let gradientStart = Color(light: 0x312E81, dark: 0x312E81)
    static let gradientEnd = Color(light: 0x818CF8, dark: 0x818CF8)

    static var gradient: LinearGradient {
        LinearGradient(
            colors: [gradientStart, gradientEnd],
            startPoint: .topLeading,
            endPoint: .bottomTrailing)
    }
}
