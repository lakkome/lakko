// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { describeVoices, stageReason, voiceLabel, type VoiceReport } from "./voice-diagnostics";

const voice = (id: string, over: Partial<VoiceReport> = {}): VoiceReport => ({
  id,
  isLocal: false,
  path: "none",
  peak: null,
  speaking: false,
  ...over,
});

describe("the voices line", () => {
  it("has the agreed shape", () => {
    expect(
      describeVoices(
        "call media interval",
        [
          voice("peer-a", { path: "stats-energy", peak: 0.314, speaking: true }),
          voice("peer-b"),
        ],
        "peer-a",
        { id: "peer-a", reason: "speaking" },
        "focus",
      ),
    ).toBe(
      "call media interval voices a=stats-energy:0.31:yes b=none:-:no floor=a stage=a(speaking) layout=focus",
    );
  });

  it("names every source, this device as me, and nothing as a dash", () => {
    expect(
      describeVoices(
        "call media interval",
        [
          voice("local", { isLocal: true, path: "analyser", peak: 0 }),
          voice("peer-a", { path: "sync-source", peak: 1 }),
          voice("peer-b", { path: "stats-level", peak: 0.019 }),
        ],
        null,
        null,
        "grid",
      ),
    ).toBe(
      "call media interval voices me=analyser:0.00:no a=sources:1.00:no b=stats-level:0.02:no floor=- stage=- layout=grid",
    );
  });

  it("labels a person by peer id, never by a name", () => {
    expect(voiceLabel("peer-7f3a", false)).toBe("7f3a");
    expect(voiceLabel("local", true)).toBe("me");
  });
});

describe("why a person is on the large tile", () => {
  const people = [
    { id: "a", camOn: true },
    { id: "b", camOn: false },
  ];
  const none = { activeSpeakerId: null, lastSpokeAtMs: {} };

  it("follows the order of the shared rule", () => {
    expect(stageReason("a", people, "a", { activeSpeakerId: "a", lastSpokeAtMs: { a: 5 } })).toBe("pinned");
    expect(stageReason("a", people, null, { activeSpeakerId: "a", lastSpokeAtMs: { a: 5 } })).toBe("speaking");
    expect(stageReason("a", people, null, { activeSpeakerId: null, lastSpokeAtMs: { a: 5 } })).toBe("recent");
    expect(stageReason("a", people, null, none)).toBe("picture");
    expect(stageReason("b", people, null, none)).toBe("first");
    // A pin on someone else does not explain this person.
    expect(stageReason("a", people, "b", none)).toBe("picture");
  });
});
