// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { callSettingsSections, type SettingsDevice } from "./call-settings";

const DEVICES: SettingsDevice[] = [
  { id: "default", label: "Default - MacBook Microphone", kind: "audioinput" },
  { id: "mic-1", label: "MacBook Microphone", kind: "audioinput" },
  { id: "mic-2", label: "AirPods", kind: "audioinput" },
  { id: "cam-1", label: "FaceTime HD Camera", kind: "videoinput" },
  { id: "out-1", label: "MacBook Speakers", kind: "audiooutput" },
  { id: "out-2", label: "", kind: "audiooutput" },
];
const NONE = { audioInput: null, videoInput: null, audioOutput: null };

describe("callSettingsSections", () => {
  it("lists microphone, camera and speaker, each starting with the system default", () => {
    const sections = callSettingsSections(DEVICES, NONE, true);
    expect(sections.map((s) => s.kind)).toEqual(["microphone", "camera", "speaker"]);
    for (const s of sections) {
      expect(s.options[0]).toEqual({ id: null, label: "System default", selected: true });
    }
  });

  it("drops the browser's own default entry, since the first row already is it", () => {
    const mic = callSettingsSections(DEVICES, NONE, true)[0];
    expect(mic?.options.map((o) => o.id)).toEqual([null, "mic-1", "mic-2"]);
  });

  it("marks the chosen device and nothing else", () => {
    const mic = callSettingsSections(DEVICES, { ...NONE, audioInput: "mic-2" }, true)[0];
    expect(mic?.options.filter((o) => o.selected).map((o) => o.id)).toEqual(["mic-2"]);
  });

  it("reads a choice the browser no longer reports as the system default", () => {
    const mic = callSettingsSections(DEVICES, { ...NONE, audioInput: "unplugged" }, true)[0];
    expect(mic?.options.filter((o) => o.selected).map((o) => o.id)).toEqual([null]);
  });

  it("has no speaker section where the browser cannot move the sound", () => {
    expect(callSettingsSections(DEVICES, NONE, false).map((s) => s.kind)).toEqual([
      "microphone",
      "camera",
    ]);
  });

  it("has no section for a kind with no device", () => {
    const onlyMic = DEVICES.filter((d) => d.kind === "audioinput");
    expect(callSettingsSections(onlyMic, NONE, true).map((s) => s.kind)).toEqual(["microphone"]);
    expect(callSettingsSections([], NONE, true)).toEqual([]);
  });

  it("numbers a device the browser gives no name", () => {
    const speaker = callSettingsSections(DEVICES, NONE, true)[2];
    expect(speaker?.options.map((o) => o.label)).toEqual([
      "System default",
      "MacBook Speakers",
      "Device 2",
    ]);
  });
});
