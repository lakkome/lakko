// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import {
  audioOutputName,
  callControls,
  nextAudioOutput,
  type CallControlId,
  type CallControlsInput,
} from "./call-controls";
import type { CallPhaseKind } from "./call-screen-model";

const BASE: CallControlsInput = {
  surface: "voice",
  phase: "connected",
  micOn: true,
  cameraOn: false,
  cameraBusy: false,
  canSelectAudioOutput: true,
  nextAudioOutputLabel: "MacBook Pro Speakers",
};

function ids(input: Partial<CallControlsInput>): CallControlId[] {
  return callControls({ ...BASE, ...input }).map((c) => c.id);
}

function control(input: Partial<CallControlsInput>, id: CallControlId) {
  const found = callControls({ ...BASE, ...input }).find((c) => c.id === id);
  if (found === undefined) throw new Error(`no control ${id}`);
  return found;
}

describe("callControls — which controls exist (section 4.1)", () => {
  it("is Speaker, Mute, Camera, End for a voice call", () => {
    expect(ids({ surface: "voice" })).toEqual(["speaker", "mic", "camera", "end"]);
  });

  it("is the same four for a video call", () => {
    // Section 4.1's table gives both kinds the same row: "People start a call
    // with their voice and decide to show something a minute later."
    expect(ids({ surface: "video" })).toEqual(["speaker", "mic", "camera", "end"]);
  });

  it("offers the camera on a voice call, which is what turns it into a video one", () => {
    expect(ids({ surface: "voice" })).toContain("camera");
    expect(control({ surface: "voice", cameraOn: false }, "camera").disabled).toBe(false);
    expect(control({ surface: "voice", cameraOn: false }, "camera").accessibleName).toBe(
      "Start camera",
    );
  });

  it("keeps the camera button when BOTH cameras are off, which is when it is the way back", () => {
    // The surface has fallen back to the portrait here. A row that followed the
    // surface would drop the button exactly when it is the only way to start a
    // camera again.
    expect(ids({ surface: "voice", cameraOn: false })).toContain("camera");
  });

  it("never puts a More menu on a 1x1 call", () => {
    for (const surface of ["voice", "video"] as const) {
      expect(ids({ surface })).not.toContain("overflow");
    }
  });

  it("keeps mute, camera, speaker and end in the row on a group call, with one overflow", () => {
    expect(ids({ surface: "group" })).toEqual([
      "speaker",
      "mic",
      "camera",
      "overflow",
      "end",
    ]);
  });

  it("gives a 1x1 call the same row whatever the surface is doing", () => {
    // The row is constant for a 1x1 call; only the group overflow varies.
    expect(ids({ surface: "voice" })).toEqual(ids({ surface: "video" }));
  });

  it("omits Speaker entirely where the browser cannot move the sound", () => {
    // Section 4.1: "omit it entirely where it does not, rather than showing a
    // dead button".
    expect(ids({ canSelectAudioOutput: false })).not.toContain("speaker");
  });

  it("omits Speaker when there is no second output to move the sound to", () => {
    expect(ids({ nextAudioOutputLabel: null })).not.toContain("speaker");
  });

  it("always ends with End, and End is the only red control", () => {
    for (const surface of ["voice", "video", "group"] as const) {
      const row = callControls({ ...BASE, surface });
      expect(row[row.length - 1]?.id).toBe("end");
      expect(row.filter((c) => c.tone === "end").map((c) => c.id)).toEqual(["end"]);
    }
  });

});

describe("callControls — the two states read the right way round (section 4.1.1)", () => {
  it("draws a live microphone as the quiet outline and a muted one as the loud solid", () => {
    expect(control({ micOn: true }, "mic").tone).toBe("outline");
    expect(control({ micOn: false }, "mic").tone).toBe("solid");
  });

  it("slashes the icon in the off state", () => {
    expect(control({ micOn: true }, "mic").icon).toBe("mic");
    expect(control({ micOn: false }, "mic").icon).toBe("micOff");
    expect(control({ cameraOn: true }, "camera").icon).toBe("video");
    expect(control({ cameraOn: false }, "camera").icon).toBe("videoOff");
  });

  it("draws a sending camera as the outline and a stopped one as the solid", () => {
    expect(control({ cameraOn: true }, "camera").tone).toBe("outline");
    expect(control({ cameraOn: false }, "camera").tone).toBe("solid");
  });

  it("reports the toggles as pressed when they are off, matching the loud state", () => {
    expect(control({ micOn: false }, "mic").pressed).toBe(true);
    expect(control({ micOn: true }, "mic").pressed).toBe(false);
  });

  it("claims no pressed state for the buttons that are not toggles", () => {
    expect(control({}, "speaker").pressed).toBeNull();
    expect(control({}, "end").pressed).toBeNull();
    expect(control({ surface: "group" }, "overflow").pressed).toBeNull();
  });
});

describe("callControls — labels and spoken names (section 4.1.1)", () => {
  it("labels the action and never the state", () => {
    expect(control({ micOn: true }, "mic").label).toBe("Mute");
    expect(control({ micOn: false }, "mic").label).toBe("Mute");
    expect(control({ cameraOn: true }, "camera").label).toBe("Camera");
    expect(control({ cameraOn: false }, "camera").label).toBe("Camera");
  });

  it("uses exactly the words section 4.1.1 lists", () => {
    const labels = callControls({ ...BASE, surface: "group" }).map((c) => c.label);
    expect(labels).toEqual(["Speaker", "Mute", "Camera", "More", "End"]);
  });

  it("names the action and the resulting state for a screen reader", () => {
    expect(control({ micOn: true }, "mic").accessibleName).toBe("Mute microphone");
    expect(control({ micOn: false }, "mic").accessibleName).toBe("Unmute microphone");
    expect(control({ cameraOn: true }, "camera").accessibleName).toBe(
      "Stop camera",
    );
    expect(control({ cameraOn: false }, "camera").accessibleName).toBe(
      "Start camera",
    );
  });

  it("says where Speaker would move the sound", () => {
    expect(control({ nextAudioOutputLabel: "AirPods Pro" }, "speaker").accessibleName).toBe(
      "Play sound through AirPods Pro",
    );
  });

  it("gives every control a non-empty spoken name", () => {
    for (const surface of ["voice", "video", "group"] as const) {
      for (const c of callControls({ ...BASE, surface })) {
        expect(c.accessibleName.trim().length).toBeGreaterThan(0);
        expect(c.label.trim().length).toBeGreaterThan(0);
      }
    }
  });
});

describe("callControls — disabled, never hidden (section 4, section 4.1.1)", () => {
  it("leaves only End active while the call is ringing out", () => {
    const row = callControls({ ...BASE, phase: "calling" });
    for (const c of row) {
      expect(c.disabled).toBe(c.id !== "end");
    }
  });

  it("still shows every control while it is inert", () => {
    expect(ids({ phase: "calling" })).toEqual([
      "speaker",
      "mic",
      "camera",
      "end",
    ]);
  });

  it("makes everything inert once the call is over", () => {
    for (const phase of ["ended", "idle"] as const) {
      for (const c of callControls({ ...BASE, phase })) {
        expect(c.disabled).toBe(true);
      }
    }
  });

  it("keeps the whole row live while connecting and reconnecting", () => {
    for (const phase of ["connecting", "connected", "reconnecting"] as CallPhaseKind[]) {
      for (const c of callControls({ ...BASE, phase })) {
        expect(c.disabled).toBe(false);
      }
    }
  });

  it("refuses a second press while the camera is being acquired", () => {
    expect(control({ cameraBusy: true }, "camera").disabled).toBe(true);
    // The rest of the row keeps working.
    expect(control({ cameraBusy: true }, "mic").disabled).toBe(false);
  });
});

describe("nextAudioOutput", () => {
  const a = { id: "a", label: "Built-in" };
  const b = { id: "b", label: "AirPods" };
  const c = { id: "c", label: "Display" };

  it("has nothing to offer with fewer than two outputs", () => {
    expect(nextAudioOutput([], null)).toBeNull();
    expect(nextAudioOutput([a], null)).toBeNull();
    expect(nextAudioOutput([a], "a")).toBeNull();
  });

  it("cycles through the outputs and wraps", () => {
    expect(nextAudioOutput([a, b, c], "a")?.id).toBe("b");
    expect(nextAudioOutput([a, b, c], "b")?.id).toBe("c");
    expect(nextAudioOutput([a, b, c], "c")?.id).toBe("a");
  });

  it("starts at the first output when nothing has been picked", () => {
    expect(nextAudioOutput([a, b], null)?.id).toBe("a");
  });

  it("treats a vanished pick as no pick rather than getting stuck", () => {
    // A headset unplugged mid-call leaves a selected id no device matches.
    expect(nextAudioOutput([a, b], "gone")?.id).toBe("a");
  });

  it("visits every output before repeating one", () => {
    const seen: string[] = [];
    let current: string | null = null;
    for (let i = 0; i < 3; i += 1) {
      const next = nextAudioOutput([a, b, c], current);
      expect(next).not.toBeNull();
      current = next?.id ?? null;
      seen.push(current ?? "");
    }
    expect([...seen].sort()).toEqual(["a", "b", "c"]);
  });
});

describe("audioOutputName", () => {
  it("prefers the browser's own label", () => {
    expect(audioOutputName({ id: "x", label: "AirPods Pro" })).toBe("AirPods Pro");
  });

  it("names the reserved default device rather than reading its id aloud", () => {
    // enumerateDevices returns blank labels until a capture permission has been
    // granted, which is the ordinary state early in a call.
    expect(audioOutputName({ id: "default", label: "" })).toBe("the default speaker");
    expect(audioOutputName({ id: "abc123", label: "   " })).toBe("the next speaker");
  });

  it("is null when there is no device", () => {
    expect(audioOutputName(null)).toBeNull();
  });
});
