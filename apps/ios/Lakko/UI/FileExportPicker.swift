// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import SwiftUI
import UIKit

/// Where a FILE leaves the app: the system's own "save a copy" picker.
///
/// A Lakko screen uses an operating-system surface only where the platform
/// leaves no other way (`.claude/skills/ux-ui/design-system.md`). An app
/// cannot write into the person's own folders by itself, so a file that
/// leaves the app has to pass through the system once; this is the narrowest
/// way through: the document picker in export mode, which asks one question
/// (where to put it) and offers nothing else. It replaced the share sheet,
/// which offered a dozen destinations for a thing the person only wanted to
/// keep.
///
/// A LINK never comes here: a link is copied, or sent to a contact from
/// inside the app.
struct FileExportPicker: UIViewControllerRepresentable {
    /// The file to hand over. A copy is exported; the original stays.
    let url: URL
    /// Called once, when the picker is finished either way.
    let onFinish: () -> Void

    func makeUIViewController(context: Context) -> UIDocumentPickerViewController {
        let picker = UIDocumentPickerViewController(forExporting: [url], asCopy: true)
        picker.delegate = context.coordinator
        return picker
    }

    func updateUIViewController(_ controller: UIDocumentPickerViewController, context: Context) {}

    func makeCoordinator() -> Coordinator { Coordinator(onFinish: onFinish) }

    final class Coordinator: NSObject, UIDocumentPickerDelegate {
        private let onFinish: () -> Void

        init(onFinish: @escaping () -> Void) {
            self.onFinish = onFinish
        }

        func documentPicker(
            _ controller: UIDocumentPickerViewController, didPickDocumentsAt urls: [URL]
        ) {
            onFinish()
        }

        func documentPickerWasCancelled(_ controller: UIDocumentPickerViewController) {
            onFinish()
        }
    }
}

/// The file an export picker is up for. Identified by its location, so a
/// second file replaces the first rather than stacking a second picker.
struct ExportedFile: Identifiable, Equatable {
    let url: URL
    var id: URL { url }
}

extension View {
    /// Present the export picker for `file` while it is set.
    func fileExport(_ file: Binding<ExportedFile?>) -> some View {
        sheet(item: file) { exported in
            FileExportPicker(url: exported.url) { file.wrappedValue = nil }
                .ignoresSafeArea()
        }
    }
}
