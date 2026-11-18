// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The desktop app's copy for the web strings that speak of a browser
 * (`docs/ux/desktop.md` section 2.2).
 *
 * The desktop app runs this client inside a system webview (ADR-0085), so a
 * sentence such as "Translation isn't available in this browser" would talk
 * about something the person never opened. Every catalog key whose English
 * text names the browser has its desktop key here, in this one table, and
 * {@link t} renders the desktop key in the desktop build. A screen keeps
 * naming the web key; the choice is made here and nowhere else.
 *
 * `desktop-copy.test.ts` fails when a catalog key that says "browser" is
 * missing from the table, or a desktop key says it.
 */

/** Web key -> the key the desktop app shows in its place. */
export const DESKTOP_COPY: Readonly<Record<string, string>> = {
  "auth.passkey.error.unsupported": "auth.passkey.error.unsupported_desktop",
  "call.error.no_media_encryption": "call.error.no_media_encryption_desktop",
  "call.screen_share.refused": "call.screen_share.refused_desktop",
  "call.screen_share.refused_macos": "call.screen_share.refused_macos_desktop",
  "chat.record.denied_camera": "chat.record.denied_camera_desktop",
  "chat.record.denied_microphone": "chat.record.denied_microphone_desktop",
  "chat.record.unsupported_video": "chat.record.unsupported_video_device",
  "chat.record.unsupported_voice": "chat.record.unsupported_voice_desktop",
  // The new device's instructions: the signed-in device may be a phone, a
  // computer or a browser, which the native wording already says.
  "device_link.code.intro": "device_link.code.intro_native",
  "device_link.request.error.no_device_key": "settings.devices.error.no_device_key_desktop",
  "devices.scan.error.camera_denied_browser": "devices.scan.error.camera_denied_desktop",
  "recovery.passkey.unsupported": "recovery.passkey.unsupported_desktop",
  "settings.account.forget_device.body": "settings.account.forget_device.body_device",
  "settings.account.forget_device.sub": "settings.account.forget_device.sub_desktop",
  "settings.devices.error.no_device_key": "settings.devices.error.no_device_key_desktop",
  "settings.passkeys.error.unsupported": "settings.passkeys.error.unsupported_desktop",
  "settings.passkeys.unsupported_browser": "settings.passkeys.unsupported_desktop",
  "settings.translation.pack.managed.web": "settings.translation.pack.managed.desktop",
  "settings.translation.unavailable.transcription_web": "settings.translation.unavailable.transcription",
  "settings.translation.unavailable.translation_web": "settings.translation.unavailable.translation",
  "translate.pack.source.web": "translate.pack.source.desktop",
};

/** The key to render for `key` on this platform. */
export function keyForPlatform(key: string, desktop: boolean): string {
  if (!desktop || !Object.hasOwn(DESKTOP_COPY, key)) return key;
  return DESKTOP_COPY[key] ?? key;
}
