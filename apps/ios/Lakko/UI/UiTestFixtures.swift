// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import SwiftUI

// Everything in this file is DEBUG-only and cannot reach a Release build.
#if DEBUG

/// A screen the `LakkoUITests` bundle can launch straight into, bypassing
/// authentication.
///
/// Why this exists. An XCUITest drives the real app, and the real app opens on
/// the Welcome screen. Reaching a chat from there means signing in, which needs
/// a real account on a real deployment and a credential typed into a field —
/// and typing a credential is something the agents working on this repository
/// are forbidden to do, quite apart from making every UI test depend on a live
/// server, a live network and a live MLS group. A test that has to sign in is a
/// test that fails for eleven reasons having nothing to do with the thing it
/// asserts.
///
/// What would be wrong with testing through login, stated plainly: the test
/// would stop being a test of the composer. It would be a test of the deployment.
///
/// So the argument below replaces the whole view tree with a screen holding the
/// PRODUCTION views and canned data. It renders ``RecordButton``,
/// ``RecordingBar``, ``VideoNoteOverlay`` and ``MediaAttachmentView``
/// themselves — never a copy of them — over a real ``ChatMediaController``. The
/// only thing the fixture supplies is the data and a port that goes nowhere:
/// every gesture rule, every state transition and every accessibility label
/// under test is the shipping one.
///
/// Rules this file keeps to:
///
///   * one entry point (``requested``) and one switch (``UiTestFixtureRoot``),
///     so the whole surface is two declarations;
///   * no production behaviour is faked, stubbed or bypassed inside the views;
///   * `#if DEBUG`, so a Release build contains none of it.
enum UiTestFixture: String {

    /// A canned incoming voice message plus the recording composer.
    case voiceBubble

    /// The launch argument that selects a fixture. Passed by the UI test as
    /// `["-LakkoUITestFixture", "voiceBubble"]`.
    static let launchArgument = "-LakkoUITestFixture"

    /// The fixture this process was launched for, or `nil` for the real app.
    ///
    /// Read from the process arguments rather than from `UserDefaults` (which
    /// would also resolve a leading-dash argument) so that nothing about the
    /// app's persisted preferences is involved in the decision.
    static var requested: UiTestFixture? {
        let arguments = ProcessInfo.processInfo.arguments
        guard let flag = arguments.firstIndex(of: launchArgument) else { return nil }
        let valueIndex = flag + 1
        guard valueIndex < arguments.count else { return nil }
        return UiTestFixture(rawValue: arguments[valueIndex])
    }
}

/// The one switch over fixture names.
struct UiTestFixtureRoot: View {
    let fixture: UiTestFixture

    var body: some View {
        switch fixture {
        case .voiceBubble:
            VoiceBubbleFixture()
        }
    }
}

/// ADR-0047 — a canned voice message and the recording composer, with nothing
/// else on screen.
///
/// The composer here is NOT a reproduction of `ChatScreen`'s: it mounts
/// ``RecordingBar`` unconditionally and lets the bar decide for itself whether
/// it has anything to show (it renders `EmptyView` while idle). `ChatScreen`
/// instead gates it behind its own `showsRecordingBar`, and copying that rule
/// into a fixture would be a second copy of it, free to drift from the first.
/// The gesture rules — which is the whole point of the fixture — are untouched:
/// ``RecordButton`` is mounted with a stable identity throughout a press, as
/// production requires.
private struct VoiceBubbleFixture: View {

    @StateObject private var media: ChatMediaController

    private let ref: AttachmentRef
    private let mediaKey: String

    init() {
        // A defaults suite of its own: a UI test must not leave the record mode
        // or the played-marker set of a real install behind it.
        let defaults = UserDefaults(suiteName: "me.lakko.uitest.fixture") ?? .standard
        let controller = ChatMediaController(
            port: .init(sendAttachment: { _ in }, sessionToken: { nil }),
            prefs: MediaPrefs(defaults: defaults)
        )
        let ref = AttachmentRef(
            blobId: Data(repeating: 0xB1, count: 32),
            key: Data(repeating: 0xB2, count: 32),
            nonce: Data(repeating: 0xB3, count: 12),
            mime: "audio/mp4",
            size: 8_192,
            name: "voice.m4a",
            kind: "voice",
            durationMs: 9_400,
            waveform: Data((0..<64).map { UInt8(($0 * 4) % 256) })
        )
        let key = ChatMediaController.key(for: ref)
        // The bubble prefetches its audio as it appears, and there is no blob
        // behind this canned ref to fetch. A file already filed under the key
        // makes `prefetchVoice` a no-op, so the bubble renders its normal face
        // instead of the download-failed one. Nothing plays it: the UI test
        // presses the record button, never the play button.
        _ = try? controller.temp.write(Data([0]), key: key, name: "voice.m4a", fallback: "voice.m4a")
        _media = StateObject(wrappedValue: controller)
        self.ref = ref
        self.mediaKey = key
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text("UI test fixture")
                .font(.footnote)
                .foregroundStyle(.secondary)
            MediaAttachmentView(ref: ref, bubble: .voice, mediaKey: mediaKey, incoming: true)
            Spacer()
            // The permission-denied card and the transient hint, in the places
            // `ChatScreen` puts them: a camera-less simulator answers the
            // mode-switch tap with a hint, and a denied microphone answers a
            // hold with the card, so both have to be on screen for a test to
            // see what the gesture did.
            if let denied = media.denied {
                MediaDeniedCard(resource: denied) { media.dismissDenied() }
            }
            if let hint = media.hint {
                Text(hint)
                    .font(.footnote)
                    .foregroundStyle(.secondary)
            }
            HStack(alignment: .bottom, spacing: 4) {
                RecordingBar(media: media)
                Spacer(minLength: 0)
                RecordButton(media: media)
            }
        }
        .padding(16)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .leading)
        .overlay { VideoNoteOverlay(media: media) }
        .environmentObject(media)
        .environmentObject(media.live)
    }
}

#endif
