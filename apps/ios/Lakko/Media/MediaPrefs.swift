// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// ADR-0047 — per-device media preferences. Mirror of
/// `apps/web/src/media/media-prefs.ts`.
///
/// All of these are conveniences a user can lose without harm, so every read
/// falls back to the spec's default:
///
///   * the composer's record mode, mic or camera (UX spec §1, "remembered per
///     device");
///   * "Autoplay video notes", default OFF (§3 — autoplay downloads media);
///   * the GIF privacy note's "Got it" (§5, "remembered per device");
///   * the local-only "played" marker on voice messages and video notes (§8 —
///     never sent, it is not a read receipt).
public struct MediaPrefs {

    public static let recordModeKey = "lakko.composer.recordMode"
    public static let autoplayKey = "lakko.media.autoplayVideoNotes"
    public static let gifNoteKey = "lakko.gif.privacyNoteDismissed"
    public static let playedKey = "lakko.media.played"
    /// The played set is capped so it cannot grow without bound; the oldest
    /// entries fall off first (an old message then shows its dot again, which
    /// is harmless).
    public static let maxPlayed = 2000

    private let defaults: UserDefaults

    public init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }

    public var recordMode: CaptureKind {
        get { defaults.string(forKey: Self.recordModeKey) == CaptureKind.videoNote.rawValue ? .videoNote : .voice }
        nonmutating set { defaults.set(newValue.rawValue, forKey: Self.recordModeKey) }
    }

    public var autoplayVideoNotes: Bool {
        get { defaults.bool(forKey: Self.autoplayKey) }
        nonmutating set { defaults.set(newValue, forKey: Self.autoplayKey) }
    }

    public var gifNoteDismissed: Bool {
        get { defaults.bool(forKey: Self.gifNoteKey) }
        nonmutating set { defaults.set(newValue, forKey: Self.gifNoteKey) }
    }

    public func playedKeys() -> [String] {
        defaults.stringArray(forKey: Self.playedKey) ?? []
    }

    public func markPlayed(_ key: String) {
        var list = playedKeys()
        guard !list.contains(key) else { return }
        list.append(key)
        if list.count > Self.maxPlayed { list.removeFirst(list.count - Self.maxPlayed) }
        defaults.set(list, forKey: Self.playedKey)
    }
}
