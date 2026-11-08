// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { callControls, type CallControlId, type CallControlsInput } from "./call-controls";
import type { CallPhaseKind } from "./call-screen-model";

const BASE: CallControlsInput = {
  surface: "voice",
  phase: "connected",
  micOn: true,
  cameraOn: false,
  cameraBusy: false,
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
  it("is Microphone, Camera, Settings, End for a voice call", () => {
    expect(ids({ surface: "voice" })).toEqual(["mic", "camera", "settings", "end"]);
  });

  it("is the same four for a video call", () => {
    // Section 4.1's table gives both kinds the same row: "People start a call
    // with their voice and decide to show something a minute later."
    expect(ids({ surface: "video" })).toEqual(["mic", "camera", "settings", "end"]);
  });

  it("has no Speaker control on any call", () => {
    // The maintainer's decision, 2026-10-02: it is not something a person
    // uses during a call, and it made the row longer.
    for (const surface of ["voice", "video", "group"] as const) {
      const row = ids({ surface, othersPresent: true, canInvite: true }) as string[];
      expect(row).not.toContain("speaker");
    }
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

  it("gives a group call Participants, and Chat only where the call carries one", () => {
    // A chat control that sent nothing anywhere was found live on 2026-10-02
    // and removed; Participants and Invite were merged the same day. The
    // call's own chat (section 4.1.4) comes with the `chat` input only.
    const row: CallControlId[] = ["mic", "camera", "participants", "settings", "end"];
    expect(ids({ surface: "group", othersPresent: true })).toEqual(row);
    expect(ids({ surface: "group", canInvite: true })).toEqual(row);
  });

  it("has no Participants button while alone in a call that has no link to hand out", () => {
    // Nothing to list and nothing to offer: the button would open nothing.
    expect(ids({ surface: "group" })).toEqual(["mic", "camera", "settings", "end"]);
  });

  it("reads Participants as pressed while the list is open, where it opens the list directly", () => {
    const direct = { surface: "group", othersPresent: true } as const;
    expect(control(direct, "participants").pressed).toBe(false);
    expect(control({ ...direct, participantsOpen: true }, "participants").pressed).toBe(true);
  });

  it("gives a 1x1 call the same row whatever the surface is doing", () => {
    // The row is constant for a 1x1 call; only the group overflow varies.
    expect(ids({ surface: "voice" })).toEqual(ids({ surface: "video" }));
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
    expect(control({}, "end").pressed).toBeNull();
    expect(control({ surface: "group" }, "end").pressed).toBeNull();
  });
});

describe("callControls — labels and spoken names (section 4.1.1)", () => {
  it("labels the action and never the state", () => {
    expect(control({ micOn: true }, "mic").label).toBe("Microphone");
    expect(control({ micOn: false }, "mic").label).toBe("Microphone");
    expect(control({ cameraOn: true }, "camera").label).toBe("Camera");
    expect(control({ cameraOn: false }, "camera").label).toBe("Camera");
  });

  it("uses exactly the words section 4.1.1 lists", () => {
    const labels = callControls({ ...BASE, surface: "group", othersPresent: true }).map(
      (c) => c.label,
    );
    expect(labels).toEqual(["Microphone", "Camera", "Participants", "Settings", "End"]);
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
  it("leaves only End and Settings active while the call is ringing out", () => {
    // Nothing to mute yet; but which microphone the call will use is worth
    // choosing before anyone answers.
    const row = callControls({ ...BASE, phase: "calling" });
    for (const c of row) {
      expect(c.disabled).toBe(c.id !== "end" && c.id !== "settings");
    }
  });

  it("still shows every control while it is inert", () => {
    expect(ids({ phase: "calling" })).toEqual(["mic", "camera", "settings", "end"]);
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

describe("callControls — Participants on a call that can invite (docs/ux/call-links.md section 3)", () => {
  const group: Partial<CallControlsInput> = { surface: "group", canInvite: true };

  it("is there from the first second, alone or not, and never on a 1x1 call", () => {
    expect(ids(group)).toContain("participants");
    expect(ids({ ...group, othersPresent: true })).toContain("participants");
    expect(ids({ surface: "voice", canInvite: true })).not.toContain("participants");
  });

  it("sits before End, so the red control keeps the far side of the row", () => {
    const row = ids(group);
    expect(row.indexOf("participants")).toBe(row.length - 3);
    expect(row[row.length - 1]).toBe("end");
  });

  it("opens a menu, so it is not a toggle that stays on", () => {
    const entry = control(group, "participants");
    expect(entry.label).toBe("Participants");
    expect(entry.pressed).toBeNull();
    expect(entry.tone).toBe("outline");
    expect(entry.icon).toBe("groups");
    // Even with the list open: the pressed state belongs to the direct mode.
    expect(control({ ...group, participantsOpen: true }, "participants").pressed).toBeNull();
  });

  it("refuses a second press while a link is still being prepared", () => {
    expect(control({ ...group, inviteBusy: true }, "participants").disabled).toBe(true);
    expect(control({ ...group, inviteBusy: false }, "participants").disabled).toBe(false);
  });

  it("goes inert once the call is over, like every other control", () => {
    for (const phase of ["ended", "idle"] as CallPhaseKind[]) {
      expect(control({ ...group, phase }, "participants").disabled).toBe(true);
    }
    // Unlike Mute and Camera, a link can be handed out while the call is
    // still starting: there is something to invite someone to from the first
    // second.
    expect(control({ ...group, phase: "calling" }, "participants").disabled).toBe(false);
  });
});

describe("callControls — Share screen (docs/ux/screen-share.md section 1)", () => {
  const can = { sharing: false, busy: false };

  it("comes after Camera in a connected call, one-to-one and group", () => {
    expect(ids({ screenShare: can })).toEqual(["mic", "camera", "screen", "settings", "end"]);
    expect(ids({ surface: "group", othersPresent: true, screenShare: can })).toEqual([
      "mic",
      "camera",
      "screen",
      "participants",
      "settings",
      "end",
    ]);
  });

  it("is absent where this device cannot send, and before the call connects", () => {
    expect(ids({})).not.toContain("screen");
    for (const phase of ["calling", "connecting", "reconnecting", "ended"] as const) {
      expect(ids({ phase, screenShare: can })).not.toContain("screen");
    }
  });

  it("is the loud state while sharing, and says Stop sharing", () => {
    const off = control({ screenShare: can }, "screen");
    expect(off.tone).toBe("outline");
    expect(off.pressed).toBe(false);
    expect(off.accessibleName).toBe("Share screen");
    const on = control({ screenShare: { sharing: true, busy: false } }, "screen");
    expect(on.tone).toBe("solid");
    expect(on.pressed).toBe(true);
    expect(on.accessibleName).toBe("Stop sharing");
    // The word under the circle names the thing, never the state.
    expect(on.label).toBe(off.label);
  });

  it("refuses a second press while the picker is open", () => {
    expect(control({ screenShare: { sharing: false, busy: true } }, "screen").disabled).toBe(true);
  });
});

describe("callControls — Chat (section 4.1.4)", () => {
  const group: Partial<CallControlsInput> = {
    surface: "group",
    othersPresent: true,
    chat: { open: false, unread: 0 },
  };

  it("sits after Participants and before Settings", () => {
    expect(ids(group)).toEqual(["mic", "camera", "participants", "chat", "settings", "end"]);
  });

  it("is there from the first second, alone in the call too", () => {
    expect(ids({ ...group, othersPresent: false })).toEqual(["mic", "camera", "chat", "settings", "end"]);
  });

  it("never appears on a one-to-one call", () => {
    for (const surface of ["voice", "video"] as const) {
      expect(ids({ ...group, surface })).not.toContain("chat");
    }
  });

  it("names the action and carries the unread count while closed", () => {
    expect(control(group, "chat").accessibleName).toBe("Open chat");
    expect(control(group, "chat").badge).toBeUndefined();
    const unread = control({ ...group, chat: { open: false, unread: 3 } }, "chat");
    expect(unread.accessibleName).toBe("Open chat, 3 unread");
    expect(unread.badge).toBe("3");
    expect(control({ ...group, chat: { open: false, unread: 12 } }, "chat").badge).toBe("9+");
  });

  it("reads Close chat and pressed while the panel is open, with no badge", () => {
    const open = control({ ...group, chat: { open: true, unread: 0 } }, "chat");
    expect(open.accessibleName).toBe("Close chat");
    expect(open.pressed).toBe(true);
    expect(open.badge).toBeUndefined();
  });

  it("keeps its label whatever the state", () => {
    expect(control(group, "chat").label).toBe("Chat");
    expect(control({ ...group, chat: { open: true, unread: 0 } }, "chat").label).toBe("Chat");
  });
});
