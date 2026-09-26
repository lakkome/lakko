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

    /// UX spec §2 "Hearing nothing" — the recording bar with the microphone
    /// reporting silence, so a test can see whether the notice reaches a screen.
    case micSilence

    /// `docs/ux/calls.md` section 4.5 — the collapsed call bar on its own.
    case callBar

    /// `docs/ux/message-actions.md` sections 2 and 3 — two messages and the
    /// per-message action menu, so a test can hold a finger on a bubble.
    case messageActions

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
        case .micSilence:
            MicrophoneSilenceFixture()
        case .callBar:
            CallBarFixture()
        case .messageActions:
            MessageActionsFixture()
        }
    }
}

/// `docs/ux/message-actions.md` — one incoming and one outgoing message, with
/// the production action menu over them.
///
/// Why a fixture. The menu opens on a press held past
/// ``MessageLongPress/thresholdSeconds``, and nothing outside XCUITest holds a
/// touch down for real (see `LakkoUITests`' own note). Everything here is the
/// shipping surface: ``MessageMenuHost`` presents it, ``MessageRow`` draws the
/// bubbles and reports their frames, and the row itself decides which actions
/// this message offers. The fixture supplies two `ChatMessage` values and three
/// readouts a test can look at — nothing else.
///
/// The readouts are outcomes, not stand-ins. `reacted` shows what the row's
/// `onReact` was handed, which is the same closure the view model is wired to in
/// production; and the clipboard line is read back out of `UIPasteboard`, so the
/// assertion is on a round trip through the real pasteboard rather than on a
/// string the fixture kept for itself.
private struct MessageActionsFixture: View {

    /// Two directions, because the menu's content differs by exactly that:
    /// Delete and Edit are offered on one and not on the other.
    private let incoming = ChatMessage(
        messageId: 101,
        direction: .incoming,
        text: "Their message",
        createdAt: "2026-09-25T10:00:00Z",
        authorKeyB64: "them"
    )
    private let outgoing = ChatMessage(
        messageId: 102,
        direction: .outgoing,
        text: "My message",
        createdAt: "2026-09-25T10:01:00Z",
        authorKeyB64: "me"
    )

    @State private var reacted: String?
    @State private var clipboard: String?
    @State private var edited = false
    @State private var deleted = false

    var body: some View {
        MessageMenuHost {
            VStack(alignment: .leading, spacing: 10) {
                Text("UI test fixture")
                    .font(.footnote)
                    .foregroundStyle(.secondary)
                row(incoming)
                row(outgoing)
                Spacer()
                if let reacted {
                    Text("fixture: reacted \(reacted)")
                }
                if let clipboard {
                    Text("fixture: clipboard \(clipboard)")
                }
                if edited {
                    Text("fixture: edit")
                }
                if deleted {
                    Text("fixture: delete")
                }
                // Reads the general pasteboard from inside the app, which is
                // where Copy wrote it. A test process reading another app's
                // pasteboard is a cross-process paste and raises the system
                // permission banner; this keeps the round trip inside one
                // process and leaves the assertion on screen.
                Button("fixture: read clipboard") {
                    clipboard = UIPasteboard.general.string
                }
                .font(.footnote)
            }
            .padding(16)
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .leading)
        }
    }

    private func row(_ message: ChatMessage) -> some View {
        MessageRow(
            message: message,
            ownIdentity: "me",
            timeLabel: "10:00",
            maxBubbleWidth: 280,
            actionsAvailable: true,
            readState: message.direction == .outgoing ? .sent : nil,
            onPreview: { _ in },
            onEdit: { edited = true },
            onDelete: { deleted = true },
            onReact: { reacted = $0 }
        )
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
            port: .init(sendAttachment: { _, _ in }, sessionToken: { nil }),
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

/// UX spec §2 "Hearing nothing" — the production ``RecordingBar`` with a take
/// running and the microphone reporting nothing.
///
/// Why a fixture and not a unit test. `ChatMediaController.recState` is
/// `private(set)` and its only writer performs the state machine's effects, so
/// the only route into a recording state is to open a real microphone — and the
/// notice then depends on what the host machine's microphone happens to hear,
/// which is the room and not the app. The two DEBUG-only seams this uses
/// (`debugSetRecState`, `debugNoteLevel`) exist for exactly this and are
/// documented where they are declared.
///
/// What is faked and what is not: the STATE is placed, and the SAMPLES are real
/// — they run through the production ``MicrophoneSilence/observe(_:level:atMs:)``
/// fold and the production `warns`-to-`micSilent` assignment, so what a test
/// then reads off the screen was decided by the shipping rule.
private struct MicrophoneSilenceFixture: View {

    @StateObject private var media: ChatMediaController

    init() {
        let defaults = UserDefaults(suiteName: "me.lakko.uitest.fixture") ?? .standard
        let controller = ChatMediaController(
            port: .init(sendAttachment: { _, _ in }, sessionToken: { nil }),
            prefs: MediaPrefs(defaults: defaults)
        )
        // A voice take, a finger still down. No effect is performed, so no
        // recorder is opened and no permission is asked for.
        controller.debugSetRecState(.recording(kind: .voice, startedAt: 0, dx: 0, dy: 0))
        // A clock the bar can print. The ticker is not running (nothing
        // dispatched), so it stays where it is put.
        controller.live.elapsedMs = 3_000
        // Silence at the first sample and silence again once the §2 window has
        // passed: the fold's own condition for the warning, fed the way the
        // recorder feeds it.
        controller.debugNoteLevel(0, atMs: 0)
        controller.debugNoteLevel(0, atMs: MicrophoneSilence.warnAfterMs)
        _media = StateObject(wrappedValue: controller)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text("UI test fixture")
                .font(.footnote)
                .foregroundStyle(.secondary)
            RecordingBar(media: media)
            HStack(spacing: 16) {
                // §2 — "A locked take is the one most worth warning about", and
                // the bar draws the notice differently there (it is not folded
                // into one accessibility label), so a test needs to reach it.
                Button("fixture: lock take") {
                    media.debugSetRecState(.locked(kind: .voice, startedAt: 0))
                }
                // §2 — "clears the moment any level is measured".
                Button("fixture: hear something") {
                    media.debugNoteLevel(0.5, atMs: MicrophoneSilence.warnAfterMs + 100)
                }
            }
            .font(.footnote)
            Spacer()
        }
        .padding(16)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .leading)
        .environmentObject(media)
        .environmentObject(media.live)
    }
}

/// `docs/ux/calls.md` section 4.5 — the production ``CollapsedCallBar`` with
/// nothing else on screen.
///
/// Needs no seam: the bar already takes its whole state as arguments, which is
/// what makes it the one piece of the collapsed call that a test can look at
/// directly. What the fixture supplies is exactly what ``CallHostOverlay``
/// supplies from a live call — and the mute toggle mirrors the same shape, since
/// the production bar shows a value the session publishes rather than one it
/// keeps.
///
/// Everything under test — the one line, the absence of an End control, the tap
/// that brings the call back, the mute button's two faces — is the shipping
/// view's own.
private struct CallBarFixture: View {

    @State private var phase: CallBarPhase = .connecting
    @State private var connectedAt: Date?
    @State private var micOn = true
    @State private var expanded = false

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text("UI test fixture")
                .font(.footnote)
                .foregroundStyle(.secondary)
            CollapsedCallBar(
                label: "Anna",
                isPerson: true,
                phase: phase,
                connectedAt: connectedAt,
                encrypted: true,
                cameraSending: false,
                micOn: micOn,
                onExpand: { expanded = true },
                onToggleMic: { micOn.toggle() })
            // Standing in for the call screen coming back, which is all
            // `onExpand` does in production (it flips `CallBarHost.collapsed`).
            if expanded {
                Text("fixture: expanded")
            }
            Button("fixture: connect") {
                phase = .connected
                connectedAt = Date()
            }
            .font(.footnote)
            Spacer()
        }
        .padding(16)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .leading)
    }
}

#endif
