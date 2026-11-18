// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import SwiftUI

// Lakko's own dialog (design system section 3, "Dialog"): a centred card over
// a dimmed screen, never the system alert or confirmation dialog.

/// One button of a ``LakkoDialog``.
struct LakkoDialogAction: Identifiable {
    enum Role: Equatable {
        /// The confirming action: accent fill.
        case primary
        /// A confirming action that destroys something: danger.
        case danger
        /// A further, non-confirming choice (a third button): elevated surface.
        case secondary
        /// Cancel: a ghost button. Tapping the dim also runs it.
        case cancel
    }

    let id = UUID()
    let title: String
    let role: Role
    var enabled: Bool = true
    let perform: () -> Void

    static func primary(_ title: String, enabled: Bool = true, perform: @escaping () -> Void) -> Self {
        Self(title: title, role: .primary, enabled: enabled, perform: perform)
    }

    static func danger(_ title: String, enabled: Bool = true, perform: @escaping () -> Void) -> Self {
        Self(title: title, role: .danger, enabled: enabled, perform: perform)
    }

    static func secondary(_ title: String, enabled: Bool = true, perform: @escaping () -> Void) -> Self {
        Self(title: title, role: .secondary, enabled: enabled, perform: perform)
    }

    static func cancel(_ title: String? = nil, perform: @escaping () -> Void = {}) -> Self {
        Self(title: title ?? Localization.text("common.cancel"), role: .cancel, perform: perform)
    }
}

/// The one optional text field of a dialog.
struct LakkoDialogField {
    let placeholder: String
    let text: Binding<String>
}

/// Text the person may want to keep: shown selectable under the message, with
/// a button that copies it.
struct LakkoDialogDetail {
    let label: String
    let text: String
    let copyTitle: String
}

/// What a dialog shows.
struct LakkoDialogContent {
    let title: String
    var message: String?
    var field: LakkoDialogField?
    var detail: LakkoDialogDetail?
    let actions: [LakkoDialogAction]
}

/// How a dialog lays out its actions (the pure rule, unit-tested).
enum LakkoDialogLayout: Equatable {
    /// One row, right-aligned, Cancel first.
    case row
    /// Full width, stacked, confirming action on top, Cancel last.
    case stacked

    static func layout(actionCount: Int) -> Self { actionCount <= 2 ? .row : .stacked }

    /// Indices of `roles` in display order for `layout`. The relative order of
    /// non-cancel actions is kept; in a stacked layout the confirming actions
    /// (primary / danger) come before the secondary ones.
    static func order(_ roles: [LakkoDialogAction.Role], layout: Self) -> [Int] {
        let indexed = Array(roles.enumerated())
        let cancels = indexed.filter { $0.element == .cancel }.map(\.offset)
        let confirming = indexed.filter { $0.element == .primary || $0.element == .danger }.map(\.offset)
        let secondary = indexed.filter { $0.element == .secondary }.map(\.offset)
        switch layout {
        case .row: return cancels + secondary + confirming
        case .stacked: return confirming + secondary + cancels
        }
    }
}

/// The dialog card over a dimmed screen. `onOutside` runs when the dim is
/// tapped (Cancel). Used directly by screens that compose their own content
/// (Profile), and by ``View/lakkoDialog(isPresented:title:message:field:actions:)``.
struct LakkoDialog<Content: View>: View {
    let onOutside: () -> Void
    @ViewBuilder let content: () -> Content

    var body: some View {
        ZStack {
            Color.black.opacity(0.35)
                .ignoresSafeArea()
                .onTapGesture(perform: onOutside)
                .accessibilityHidden(true)
            VStack(spacing: 12) {
                content()
            }
            .padding(20)
            .frame(maxWidth: 440)
            .background(
                RoundedRectangle(cornerRadius: LakkoRadius.card, style: .continuous)
                    .fill(Color.lakkoSurface)
                    .shadow(color: .black.opacity(0.18), radius: 16, y: 6))
            .padding(.horizontal, 24)
            .accessibilityAddTraits(.isModal)
            .accessibilityAction(.escape, onOutside)
        }
        .transition(.opacity)
    }
}

/// The standard dialog: title, optional body, optional field, actions.
struct LakkoStandardDialog: View {
    let content: LakkoDialogContent
    /// Closes the dialog (sets the presenting binding to false).
    let close: () -> Void

    @FocusState private var fieldFocused: Bool

    var body: some View {
        LakkoDialog(onOutside: cancel) {
            VStack(alignment: .leading, spacing: 8) {
                Text(content.title)
                    .font(.headline)
                    .foregroundStyle(Color.lakkoText)
                    .fixedSize(horizontal: false, vertical: true)
                    .accessibilityAddTraits(.isHeader)
                if let message = content.message, !message.isEmpty {
                    Text(message)
                        .font(.subheadline)
                        .foregroundStyle(Color.lakkoTextMuted)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            if let field = content.field {
                TextField(field.placeholder, text: field.text)
                    .focused($fieldFocused)
                    .padding(.horizontal, 12)
                    .frame(minHeight: 44)
                    .background(Color.lakkoSurfaceSunken)
                    .clipShape(RoundedRectangle(cornerRadius: LakkoRadius.control))
                    .onAppear { fieldFocused = true }
            }
            if let detail = content.detail {
                VStack(alignment: .leading, spacing: 4) {
                    Text(detail.label)
                        .font(.footnote)
                        .foregroundStyle(Color.lakkoTextMuted)
                    ScrollView {
                        Text(detail.text)
                            .font(.subheadline)
                            .foregroundStyle(Color.lakkoText)
                            .textSelection(.enabled)
                            .frame(maxWidth: .infinity, alignment: .leading)
                    }
                    .frame(maxHeight: 160)
                    Button(detail.copyTitle) { UIPasteboard.general.string = detail.text }
                        .font(.footnote.weight(.medium))
                        .frame(minHeight: 44)
                        .contentShape(Rectangle())
                }
            }
            actions
                .padding(.top, 4)
        }
    }

    @ViewBuilder
    private var actions: some View {
        let layout = LakkoDialogLayout.layout(actionCount: content.actions.count)
        if layout == .row {
            ViewThatFits(in: .horizontal) {
                row
                stacked
            }
        } else {
            stacked
        }
    }

    private var row: some View {
        let order = LakkoDialogLayout.order(content.actions.map(\.role), layout: .row)
        return HStack(spacing: 8) {
            Spacer(minLength: 0)
            ForEach(order, id: \.self) { index in
                button(content.actions[index], fullWidth: false)
            }
        }
    }

    private var stacked: some View {
        let order = LakkoDialogLayout.order(content.actions.map(\.role), layout: .stacked)
        return VStack(spacing: 8) {
            ForEach(order, id: \.self) { index in
                button(content.actions[index], fullWidth: true)
            }
        }
    }

    private func button(_ action: LakkoDialogAction, fullWidth: Bool) -> some View {
        Button {
            // The action first, as the system alert does: a presenting binding
            // whose setter means "cancelled" must not run before the choice.
            action.perform()
            close()
        } label: {
            Text(action.title)
                .font(.body.weight(.medium))
                .lineLimit(1)
                .foregroundStyle(foreground(action.role))
                .padding(.horizontal, 16)
                .frame(maxWidth: fullWidth ? .infinity : nil, minHeight: 44)
                .background(background(action.role))
                .clipShape(RoundedRectangle(cornerRadius: LakkoRadius.control))
                .overlay {
                    if action.role == .secondary {
                        RoundedRectangle(cornerRadius: LakkoRadius.control)
                            .stroke(Color.lakkoBorder, lineWidth: 1)
                    }
                }
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .disabled(!action.enabled)
        .opacity(action.enabled ? 1 : 0.5)
    }

    private func foreground(_ role: LakkoDialogAction.Role) -> Color {
        switch role {
        case .primary, .danger: return .white
        case .secondary: return Color.lakkoAccent
        case .cancel: return Color.lakkoTextMuted
        }
    }

    private func background(_ role: LakkoDialogAction.Role) -> Color {
        switch role {
        case .primary: return Color.lakkoAccent
        case .danger: return Color.lakkoDanger
        case .secondary: return Color.lakkoSurface
        case .cancel: return .clear
        }
    }

    private func cancel() {
        content.actions.first(where: { $0.role == .cancel })?.perform()
        close()
    }
}

// MARK: - Presentation

/// Presents a dialog as a transparent full-screen cover, so it lies over the
/// navigation bar, the tab bar and any sheet the presenting view sits in. The
/// cover itself appears without the slide; the dim and card fade in.
private struct LakkoDialogPresenter: ViewModifier {
    @Binding var isPresented: Bool
    let make: () -> LakkoDialogContent?

    @State private var shown = false

    func body(content: Content) -> some View {
        content
            .onChange(of: isPresented, initial: true) { _, value in
                var transaction = Transaction()
                transaction.disablesAnimations = true
                withTransaction(transaction) { shown = value }
            }
            .fullScreenCover(isPresented: Binding(
                get: { shown },
                set: { value in
                    shown = value
                    if !value, isPresented { isPresented = false }
                })
            ) {
                LakkoDialogCover(make: make, close: { isPresented = false })
                    .presentationBackground(.clear)
            }
    }
}

private struct LakkoDialogCover: View {
    let make: () -> LakkoDialogContent?
    let close: () -> Void

    @State private var visible = false

    var body: some View {
        ZStack {
            if visible, let content = make() {
                LakkoStandardDialog(content: content, close: close)
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .onAppear { withAnimation(.easeOut(duration: 0.15)) { visible = true } }
    }
}

/// The item form keeps the last non-nil item, so the content stays drawn
/// while the cover goes away.
private struct LakkoItemDialogPresenter<Item>: ViewModifier {
    @Binding var item: Item?
    let make: (Item) -> LakkoDialogContent

    @State private var last: Item?

    func body(content: Content) -> some View {
        content
            .onChange(of: item != nil, initial: true) { _, present in
                if present { last = item }
            }
            .modifier(LakkoDialogPresenter(
                isPresented: Binding(get: { item != nil }, set: { if !$0 { item = nil } }),
                make: { (item ?? last).map(make) }))
    }
}

/// An action sheet: Lakko's own bottom sheet of rows (``LakkoMenuSheet``),
/// with an optional heading. Dismissing it without picking a row is Cancel.
private struct LakkoActionSheetPresenter: ViewModifier {
    @Binding var isPresented: Bool
    let title: String?
    let message: String?
    let onCancel: (() -> Void)?
    let items: () -> [LakkoMenuItem]

    /// The row the person picked; it runs once the sheet has gone, so a row
    /// that presents something else (a sheet, a picker) is not refused by a
    /// presentation still in flight.
    @State private var pending: (() -> Void)?
    @State private var picked = false

    func body(content: Content) -> some View {
        content
            .sheet(isPresented: $isPresented, onDismiss: {
                let run = pending
                pending = nil
                if picked { run?() } else { onCancel?() }
                picked = false
            }) {
                LakkoMenuSheet(items: heading + deferring(items())) {}
            }
    }

    private func deferring(_ items: [LakkoMenuItem]) -> [LakkoMenuItem] {
        items.map { item in
            switch item.kind {
            case let .action(icon, destructive, enabled, perform):
                return .action(item.title, icon: icon, destructive: destructive,
                               enabled: enabled) { pick(perform) }
            case let .choice(checked, perform):
                return .choice(item.title, checked: checked) { pick(perform) }
            case let .submenu(icon, sub):
                return .submenu(item.title, icon: icon, items: deferring(sub))
            case .text:
                return item
            }
        }
    }

    private func pick(_ perform: @escaping () -> Void) {
        pending = perform
        picked = true
        isPresented = false
    }

    private var heading: [LakkoMenuItem] {
        var rows: [LakkoMenuItem] = []
        if let title, !title.isEmpty { rows.append(.text(title)) }
        if let message, !message.isEmpty { rows.append(.text(message, secondary: true)) }
        return rows
    }
}

extension View {
    /// Lakko's own dialog (UX: no system UI surfaces). An action closes the
    /// dialog and then runs; tapping the dim runs the cancel action.
    func lakkoDialog(
        isPresented: Binding<Bool>, title: String, message: String? = nil,
        field: LakkoDialogField? = nil, actions: @escaping () -> [LakkoDialogAction]
    ) -> some View {
        modifier(LakkoDialogPresenter(isPresented: isPresented, make: {
            LakkoDialogContent(title: title, message: message, field: field, actions: actions())
        }))
    }

    /// Lakko's own dialog for an optional item; nil hides it.
    func lakkoDialog<Item>(
        item: Binding<Item?>, content: @escaping (Item) -> LakkoDialogContent
    ) -> some View {
        modifier(LakkoItemDialogPresenter(item: item, make: content))
    }

    /// Lakko's own action sheet (a choice among actions, no further input).
    func lakkoActionSheet(
        isPresented: Binding<Bool>, title: String? = nil, message: String? = nil,
        onCancel: (() -> Void)? = nil, items: @escaping () -> [LakkoMenuItem]
    ) -> some View {
        modifier(LakkoActionSheetPresenter(
            isPresented: isPresented, title: title, message: message, onCancel: onCancel, items: items))
    }
}

// MARK: - Picker

/// One value of a ``LakkoPicker``.
struct LakkoPickerOption<Value: Hashable>: Identifiable {
    let value: Value
    let label: String
    var id: Value { value }
}

/// Lakko's own picker (design system "Picker"): a row with the label and a
/// field-shaped button showing the current value; activating it opens the
/// action-sheet rows with a check mark on the current value.
struct LakkoPicker<Value: Hashable>: View {
    let title: String
    var icon: LkIcon?
    @Binding var selection: Value
    let options: [LakkoPickerOption<Value>]

    @State private var open = false
    @Environment(\.isEnabled) private var isEnabled

    init(
        _ title: String, icon: LkIcon? = nil, selection: Binding<Value>,
        options: [LakkoPickerOption<Value>]
    ) {
        self.title = title
        self.icon = icon
        self._selection = selection
        self.options = options
    }

    var body: some View {
        HStack(spacing: 12) {
            if let icon {
                Label(title, icon: icon)
            } else {
                Text(title)
            }
            Spacer(minLength: 8)
            Button { open = true } label: {
                HStack(spacing: 6) {
                    Text(currentLabel)
                        .lineLimit(1)
                    LkIconView(.chevronUpDown, size: 14, relativeTo: .footnote)
                        .foregroundStyle(Color.lakkoTextMuted)
                }
                .foregroundStyle(Color.lakkoText)
                .padding(.horizontal, 12)
                .frame(minHeight: 40)
                .background(Color.lakkoSurfaceSunken)
                .clipShape(RoundedRectangle(cornerRadius: LakkoRadius.control))
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .opacity(isEnabled ? 1 : 0.5)
            .accessibilityLabel(title)
            .accessibilityValue(currentLabel)
        }
        .lakkoActionSheet(isPresented: $open, title: title) {
            options.map { option in
                LakkoMenuItem.choice(option.label, checked: option.value == selection) {
                    selection = option.value
                }
            }
        }
    }

    private var currentLabel: String {
        options.first(where: { $0.value == selection })?.label ?? ""
    }
}

/// The inline form of a picker inside a list section: one row per value,
/// a check mark on the current one.
struct LakkoInlinePicker<Value: Hashable>: View {
    @Binding var selection: Value
    let options: [LakkoPickerOption<Value>]

    var body: some View {
        ForEach(options) { option in
            Button { selection = option.value } label: {
                HStack {
                    Text(option.label).foregroundStyle(Color.lakkoText)
                    Spacer()
                    if option.value == selection {
                        LkIconView(.checkmark, size: 20)
                            .foregroundStyle(Color.lakkoAccent)
                    }
                }
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .accessibilityAddTraits(option.value == selection ? .isSelected : [])
        }
    }
}

/// The segmented form of a picker: two to four short values side by side on
/// a sunken track, the current one raised. Drawn by the app, never the
/// system segmented control.
struct LakkoSegmentedPicker<Value: Hashable>: View {
    @Binding var selection: Value
    let options: [LakkoPickerOption<Value>]

    @Environment(\.isEnabled) private var isEnabled

    var body: some View {
        HStack(spacing: 2) {
            ForEach(options) { option in
                let selected = option.value == selection
                Button { selection = option.value } label: {
                    Text(option.label)
                        .font(.subheadline.weight(selected ? .semibold : .regular))
                        .lineLimit(1)
                        .minimumScaleFactor(0.8)
                        .foregroundStyle(selected ? Color.lakkoText : Color.lakkoTextMuted)
                        .frame(maxWidth: .infinity, minHeight: 32)
                        .background {
                            if selected {
                                RoundedRectangle(cornerRadius: LakkoRadius.control - 2)
                                    .fill(Color.lakkoSurface)
                                    .shadow(color: .black.opacity(0.08), radius: 2, y: 1)
                            }
                        }
                        .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .accessibilityAddTraits(selected ? .isSelected : [])
            }
        }
        .padding(2)
        .background(Color.lakkoSurfaceSunken)
        .clipShape(RoundedRectangle(cornerRadius: LakkoRadius.control))
        .opacity(isEnabled ? 1 : 0.5)
    }
}
