// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The call settings sheet: which microphone, camera and speaker the call uses
 * (`docs/ux/calls.md` section 4.1).
 *
 * The sheet is the app's own list, never the browser's `<select>`, which opens
 * a system picker on a phone. Each section is a group of radio rows: the
 * system default first, then every device the browser reports.
 *
 * A section exists only where choosing does something. The speaker section
 * needs a browser that can move a call's sound (`setSinkId`); without it the
 * section is absent rather than a list whose rows change nothing. A section
 * with no device reported is absent too: before a capture permission is
 * granted the browser lists none, and during a call it has been granted.
 *
 * Pure: the route passes the devices in and renders the answer.
 */

import { t } from "../l10n";

/** A device as the store holds it. */
export interface SettingsDevice {
  readonly id: string;
  readonly label: string;
  readonly kind: "audioinput" | "videoinput" | "audiooutput";
}

/** What is chosen now; `null` is the system default. */
export interface SettingsSelection {
  readonly audioInput: string | null;
  readonly videoInput: string | null;
  readonly audioOutput: string | null;
}

export type SettingsSectionKind = "microphone" | "camera" | "speaker";

/** One row of a section. `id` of `null` is the system default. */
export interface SettingsOption {
  readonly id: string | null;
  readonly label: string;
  readonly selected: boolean;
}

export interface SettingsSection {
  readonly kind: SettingsSectionKind;
  readonly title: string;
  readonly options: readonly SettingsOption[];
}

const KIND_OF: Record<SettingsSectionKind, SettingsDevice["kind"]> = {
  microphone: "audioinput",
  camera: "videoinput",
  speaker: "audiooutput",
};

function titleOf(kind: SettingsSectionKind): string {
  switch (kind) {
    case "microphone":
      return t("call.settings.microphone");
    case "camera":
      return t("call.control.camera");
    case "speaker":
      return t("call.control.speaker");
  }
}

function chosenOf(kind: SettingsSectionKind, selection: SettingsSelection): string | null {
  switch (kind) {
    case "microphone":
      return selection.audioInput;
    case "camera":
      return selection.videoInput;
    case "speaker":
      return selection.audioOutput;
  }
}

/**
 * The sections of the sheet, in order: microphone, camera, speaker.
 *
 * The browser's own `default` entry is dropped, since the first row already
 * is the system default and two rows for one thing would be a choice between
 * nothing. A device the browser gives no name is numbered. A stored choice
 * the browser no longer reports (a headset was unplugged) reads as the system
 * default, which is what the call is in fact using.
 */
export function callSettingsSections(
  devices: readonly SettingsDevice[],
  selection: SettingsSelection,
  canSelectAudioOutput: boolean,
): SettingsSection[] {
  const out: SettingsSection[] = [];
  for (const kind of ["microphone", "camera", "speaker"] as const) {
    if (kind === "speaker" && !canSelectAudioOutput) continue;
    const listed = devices.filter(
      (d) => d.kind === KIND_OF[kind] && d.id !== "" && d.id !== "default",
    );
    if (listed.length === 0) continue;
    const stored = chosenOf(kind, selection);
    const chosen = listed.some((d) => d.id === stored) ? stored : null;
    out.push({
      kind,
      title: titleOf(kind),
      options: [
        {
          id: null,
          label: t("call.settings.system_default"),
          selected: chosen === null,
        },
        ...listed.map((d, i) => ({
          id: d.id,
          label:
            d.label.trim() !== ""
              ? d.label.trim()
              : t("call.settings.unnamed_device", { n: String(i + 1) }),
          selected: chosen === d.id,
        })),
      ],
    });
  }
  return out;
}
