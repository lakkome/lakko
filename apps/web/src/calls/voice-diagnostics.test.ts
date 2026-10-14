// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import {
  describePaths,
  describeVoices,
  stageReason,
  voiceLabel,
  type VoiceReport,
} from "./voice-diagnostics";

const voice = (id: string, over: Partial<VoiceReport> = {}): VoiceReport => ({
  id,
  isLocal: false,
  path: "none",
  peak: null,
  speaking: false,
  threshold: 0.02,
  analyser: null,
  analyserStuck: false,
  receiver: null,
  ...over,
});

describe("the voices line", () => {
  it("has the agreed shape", () => {
    expect(
      describeVoices(
        "call media interval",
        [
          voice("peer-a", {
            path: "analyser",
            peak: 0.0314,
            speaking: true,
            threshold: 0.012,
            analyser: 0.0314,
            receiver: { energy: 0.04, statsNote: "ok", statsLevel: 0.12, sources: 0.03 },
          }),
          voice("peer-b"),
        ],
        "peer-a",
        { id: "peer-a", reason: "speaking" },
        "focus",
      ),
    ).toBe(
      "call media interval voices " +
        "a=analyser:0.031:yes[thr=0.012,an=0.031,en=0.040,lv=0.120,src=0.030] " +
        "b=none:-:no[thr=0.020,an=-] " +
        "floor=a stage=a(speaking) layout=focus",
    );
  });

  it("names this device as me with its one path, and keeps one token per person", () => {
    const line = describeVoices(
      "call media interval",
      [
        voice("local", { isLocal: true, path: "analyser", peak: 0, analyser: 0 }),
        voice("peer-a", {
          path: "stats-energy",
          peak: 1,
          analyser: 0,
          analyserStuck: true,
          receiver: { energy: 1, statsNote: "ok", statsLevel: null, sources: null },
        }),
      ],
      null,
      null,
      "grid",
    );
    expect(line).toBe(
      "call media interval voices " +
        "me=analyser:0.000:no[thr=0.020,an=0.000] " +
        "a=stats-energy:1.000:no[thr=0.020,an=0.000!,en=1.000,lv=-,src=-] " +
        "floor=- stage=- layout=grid",
    );
    // prefix (3 words) + voices + two people + floor + stage + layout.
    expect(line.split(" ")).toHaveLength(9);
  });

  it("shows the raw value of every path tried, and why the energy path had none", () => {
    // Firefox as the 2026-10-11 log left it: nothing on the sources, and
    // whatever the statistics turn out to carry.
    expect(
      describePaths(
        voice("peer-a", {
          analyser: 0.05,
          receiver: { energy: null, statsNote: "no-energy", statsLevel: 0.4, sources: null },
        }),
      ),
    ).toBe("[thr=0.020,an=0.050,en=-(no-energy),lv=0.400,src=-]");
    for (const note of ["pending", "failed", "no-record", "first"] as const) {
      expect(
        describePaths(
          voice("peer-a", { receiver: { energy: null, statsNote: note, statsLevel: null, sources: 0.03 } }),
        ),
      ).toBe(`[thr=0.020,an=-,en=-(${note}),lv=-,src=0.030]`);
    }
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
