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

    /// `docs/ux/calls.md` section 4.5 — the collapsed call window on its own,
    /// floating over an empty screen. (The launch value keeps the name it had
    /// when the collapsed call was a bar.)
    case callBar

    /// `docs/ux/message-actions.md` sections 2 and 3 — two messages and the
    /// per-message action menu, so a test can hold a finger on a bubble.
    case messageActions

    /// `docs/ux/calls.md` section 4.1 and `docs/ux/call-backgrounds.md` — the
    /// call's settings sheet on its own, with the camera background section.
    case callSettings

    /// `docs/ux/attachments.md` section 2 — the paperclip menu of a group,
    /// presented as the sheet a conversation presents.
    case attachmentMenu

    /// `docs/ux/attachments.md` sections 7.1 and 7.1a — the poll editor.
    case pollEditor

    /// `docs/ux/attachments.md` sections 8.1 and 7.1a — the checklist editor.
    case checklistEditor

    /// `docs/ux/attachments.md` section 2.5 — the production composer field,
    /// with a line saying what its last media paste carried.
    case composerPaste

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
        case .callSettings:
            CallSettingsSheet(state: CallUiState(), onDone: {})
        case .attachmentMenu:
            AttachmentMenuFixture()
        case .pollEditor:
            PollEditorScreen(anonymity: .switchDefaultOn, onCreate: { _ in }, onCancel: {})
        case .checklistEditor:
            ChecklistEditorScreen(initial: nil, onDone: { _ in }, onCancel: {})
        case .composerPaste:
            ComposerPasteFixture()
        }
    }
}

/// The production composer field. A media paste reads what it carried through
/// the production loader and names it in a line a test can find.
private struct ComposerPasteFixture: View {
    @State private var draft = ""
    @State private var focused = false
    @State private var pasted = "none"
    @StateObject private var caret = ComposerCaret()

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text("UI test fixture")
                .font(.footnote)
                .foregroundStyle(.secondary)
            Text("pasted: \(pasted)")
                .accessibilityIdentifier("composer-paste-result")
            Spacer()
            ComposerTextField(
                text: $draft, caret: caret, disabled: false, focus: $focused,
                onPasteMedia: { providers in
                    Task {
                        let loaded = await PastedMedia.load(providers)
                        pasted = loaded.map { item -> String in
                            switch item {
                            case .photo(let data): return "photo \(data.count)"
                            case .video: return "video"
                            case .failed: return "failed"
                            }
                        }.joined(separator: ", ")
                    }
                })
                .accessibilityIdentifier("composer-field")
        }
        .padding(16)
    }
}

/// The production attachment menu of a group member, shown in the sheet a
/// conversation presents it in.
private struct AttachmentMenuFixture: View {
    @State private var open = true

    var body: some View {
        Color.lakkoBackground
            .ignoresSafeArea()
            .sheet(isPresented: $open) {
                AttachmentMenuSheet(rows: attachmentMenu(
                    kind: .group, role: .member, hasCamera: true, hasLocation: true, callHoldsCamera: false)
                ) { _ in }
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
    /// The production edit sheet, opened by Edit: its field must take the
    /// keyboard by itself.
    @State private var editing = false
    @State private var editText = ""
    @State private var savedEdit: String?

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
                if let savedEdit {
                    Text("fixture: saved \(savedEdit)")
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
        .sheet(isPresented: $editing) {
            EditMessageSheet(
                text: $editText,
                onCancel: { editing = false },
                onSave: {
                    savedEdit = editText
                    editing = false
                })
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
            onEdit: {
                edited = true
                editText = message.text
                editing = true
            },
            onDelete: { deleted = true },
            onReact: { reacted = $0 },
            // The row production draws (`ChatViewModel.reactionRow(for:)`):
            // the shared rule over a conversation that takes any reaction,
            // with no quick reaction chosen and no recent ones, so the row is
            // the six defaults. Left out, the row is empty and the menu shows
            // no reactions at all.
            reactionRow: ReactionRules.row(
                message: message, own: "me", quick: nil, recent: [], allowed: .all)
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

/// `docs/ux/calls.md` section 4.5 — the production ``FloatingCallWindow``
/// over an otherwise empty screen.
///
/// What the fixture supplies is exactly what ``CallHostOverlay`` supplies from
/// a live call: a ``CallUiState`` with this device and one other person in it,
/// the phase, when the call connected, and the two actions. The mute toggle
/// writes the local participant's `micOn`, which is the whole mute control on
/// the group path too.
///
/// Everything under test — the one line, the absence of an End control, the
/// tap that brings the call back, the mute button's two faces, the drag to a
/// corner and the tuck — is the shipping view's own.
private struct CallBarFixture: View {

    @StateObject private var state = CallBarFixture.makeState()
    @State private var phase: CallBarPhase = .connecting
    @State private var connectedAt: Date?
    @State private var expanded = false

    private static func makeState() -> CallUiState {
        let state = CallUiState()
        state.setE2ee(CallUiState.E2eeState(active: true, shortFingerprint: nil))
        state.setParticipants([
            CallUiState.Participant(
                id: "local", displayName: "", isLocal: true, micOn: true, camOn: false,
                handRaised: false, quality: .good, speaking: false, screenSharing: false),
            CallUiState.Participant(
                id: "remote", displayName: "Anna", isLocal: false, micOn: true, camOn: false,
                handRaised: false, quality: .good, speaking: false, screenSharing: false),
        ])
        return state
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text("UI test fixture")
                .font(.footnote)
                .foregroundStyle(.secondary)
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
        .overlay {
            FloatingCallWindow(
                state: state,
                groupCall: false,
                label: "Anna",
                phase: phase,
                connectedAt: connectedAt,
                peerKey: nil,
                onExpand: { expanded = true },
                onToggleMic: {
                    let micOn = state.participants.first(where: { $0.isLocal })?.micOn ?? true
                    state.setMic(!micOn)
                })
        }
    }
}

#endif
