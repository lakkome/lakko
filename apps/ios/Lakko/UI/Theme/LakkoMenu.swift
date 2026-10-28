// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import SwiftUI

/// One row of Lakko's own long-press menu (never the system context menu).
struct LakkoMenuItem: Identifiable {
    enum Kind {
        /// A tappable action.
        case action(systemImage: String?, destructive: Bool, enabled: Bool, perform: () -> Void)
        /// A line of plain text (a heading, a name, a limit note).
        case text(secondary: Bool)
        /// A second level of the same sheet: tapping replaces the rows with
        /// `items` and a Back row.
        case submenu(systemImage: String?, items: [LakkoMenuItem])
        /// One value of a picker: a check mark when it is the current one.
        case choice(checked: Bool, perform: () -> Void)
    }

    let id = UUID()
    let title: String
    let kind: Kind

    static func action(
        _ title: String, systemImage: String? = nil, destructive: Bool = false,
        enabled: Bool = true, perform: @escaping () -> Void
    ) -> LakkoMenuItem {
        LakkoMenuItem(title: title, kind: .action(
            systemImage: systemImage, destructive: destructive, enabled: enabled, perform: perform))
    }

    static func text(_ title: String, secondary: Bool = false) -> LakkoMenuItem {
        LakkoMenuItem(title: title, kind: .text(secondary: secondary))
    }

    static func submenu(_ title: String, systemImage: String? = nil, items: [LakkoMenuItem]) -> LakkoMenuItem {
        LakkoMenuItem(title: title, kind: .submenu(systemImage: systemImage, items: items))
    }

    static func choice(_ title: String, checked: Bool, perform: @escaping () -> Void) -> LakkoMenuItem {
        LakkoMenuItem(title: title, kind: .choice(checked: checked, perform: perform))
    }
}

/// A long press (the message menu's threshold) opens an in-app sheet of
/// `items`; tapping an action closes it first. No items, no menu.
private struct LakkoLongPressMenu: ViewModifier {
    let items: () -> [LakkoMenuItem]
    @State private var shown: [LakkoMenuItem]?

    func body(content: Content) -> some View {
        content
            .simultaneousGesture(
                LongPressGesture(minimumDuration: MessageLongPress.thresholdSeconds).onEnded { _ in
                    let list = items()
                    if !list.isEmpty { shown = list }
                })
            .sheet(isPresented: Binding(get: { shown != nil }, set: { if !$0 { shown = nil } })) {
                LakkoMenuSheet(items: shown ?? []) { shown = nil }
            }
    }
}

/// The sheet the long-press menu opens: rows in the app's own style. A
/// submenu replaces the rows with its own and a Back row.
struct LakkoMenuSheet: View {
    let items: [LakkoMenuItem]
    let dismiss: () -> Void

    @State private var stack: [[LakkoMenuItem]] = []
    /// The measured height of the rows, so a long heading is not clipped.
    @State private var measured: CGFloat?

    private var shown: [LakkoMenuItem] { stack.last ?? items }

    var body: some View {
        VStack(spacing: 0) {
            if !stack.isEmpty {
                Button { stack.removeLast() } label: {
                    label(Localization.text("common.back"), systemImage: "chevron.backward",
                          tint: Color.lakkoAccent, trailing: nil)
                }
                .buttonStyle(.plain)
                Divider()
            }
            ForEach(shown) { item in
                row(item)
                if item.id != shown.last?.id { Divider().padding(.leading, 20) }
            }
        }
        .padding(.vertical, 12)
        .fixedSize(horizontal: false, vertical: true)
        .background(GeometryReader { proxy in
            Color.clear
                .onAppear { measured = proxy.size.height }
                .onChange(of: proxy.size.height) { _, height in measured = height }
        })
        .frame(maxHeight: .infinity, alignment: .top)
        .presentationDetents([.height(
            (measured.map { $0 + 24 }) ?? Self.height(rows: shown.count + (stack.isEmpty ? 0 : 1)))])
        .presentationDragIndicator(.visible)
    }

    static func height(rows: Int) -> CGFloat { CGFloat(rows) * 52 + 40 }

    @ViewBuilder
    private func row(_ item: LakkoMenuItem) -> some View {
        switch item.kind {
        case let .action(systemImage, destructive, enabled, perform):
            Button {
                dismiss()
                perform()
            } label: {
                label(item.title, systemImage: systemImage,
                      tint: destructive ? Color.lakkoDanger : Color.lakkoText, trailing: nil)
            }
            .buttonStyle(.plain)
            .disabled(!enabled)
            .opacity(enabled ? 1 : 0.4)
        case let .submenu(systemImage, items):
            Button { stack.append(items) } label: {
                label(item.title, systemImage: systemImage, tint: Color.lakkoText, trailing: "chevron.forward")
            }
            .buttonStyle(.plain)
        case let .choice(checked, perform):
            Button {
                dismiss()
                perform()
            } label: {
                HStack(spacing: 12) {
                    Text(item.title)
                    Spacer()
                    if checked {
                        Image(systemName: "checkmark")
                            .font(.body.weight(.semibold))
                            .foregroundStyle(Color.lakkoAccent)
                    }
                }
                .font(.body)
                .foregroundStyle(Color.lakkoText)
                .padding(.horizontal, 20)
                .frame(minHeight: 52)
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .accessibilityAddTraits(checked ? .isSelected : [])
        case .text(let secondary):
            Text(item.title)
                .font(secondary ? .footnote : .body.weight(.semibold))
                .foregroundStyle(secondary ? Color.lakkoTextMuted : Color.lakkoText)
                .fixedSize(horizontal: false, vertical: true)
                .padding(.vertical, 8)
                .frame(maxWidth: .infinity, minHeight: secondary ? 0 : 52, alignment: .leading)
                .padding(.horizontal, 20)
        }
    }

    private func label(_ title: String, systemImage: String?, tint: Color, trailing: String?) -> some View {
        HStack(spacing: 12) {
            if let systemImage { Image(systemName: systemImage).frame(width: 24) }
            Text(title)
            Spacer()
            if let trailing {
                Image(systemName: trailing).font(.footnote).foregroundStyle(Color.lakkoTextMuted)
            }
        }
        .font(.body)
        .foregroundStyle(tint)
        .padding(.horizontal, 20)
        .frame(minHeight: 52)
        .contentShape(Rectangle())
    }
}

extension View {
    /// Lakko's own long-press menu (UX: no system UI surfaces).
    func lakkoLongPressMenu(_ items: @escaping () -> [LakkoMenuItem]) -> some View {
        modifier(LakkoLongPressMenu(items: items))
    }
}
