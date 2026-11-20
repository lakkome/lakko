// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { boundTag, parseId3, parseMp4Tags, readAudioTags, titleFromFileName } from "./audio-tags";

const enc = new TextEncoder();

function syncsafe(n: number): number[] {
  return [(n >> 21) & 0x7f, (n >> 14) & 0x7f, (n >> 7) & 0x7f, n & 0x7f];
}
function be32(n: number): number[] {
  return [(n >>> 24) & 0xff, (n >> 16) & 0xff, (n >> 8) & 0xff, n & 0xff];
}

function frame(id: string, data: number[], major: 3 | 4): number[] {
  const size = major === 4 ? syncsafe(data.length) : be32(data.length);
  return [...enc.encode(id), ...size, 0, 0, ...data];
}

function id3(major: 3 | 4, frames: number[][]): Uint8Array {
  const body = frames.flat();
  return new Uint8Array([...enc.encode("ID3"), major, 0, 0, ...syncsafe(body.length), ...body, 0xff, 0xfb]);
}

describe("audio tags", () => {
  it("reads ID3v2.3 UTF-8 and latin1 text frames", () => {
    const b = id3(3, [frame("TIT2", [3, ...enc.encode("Song")], 3), frame("TPE1", [0, 0x42, 0xe9], 3)]);
    expect(parseId3(b)).toEqual({ title: "Song", artist: "Bé" });
  });
  it("reads ID3v2.4 UTF-16 text and an APIC cover", () => {
    const utf16 = [0xff, 0xfe, 0x48, 0x00, 0x69, 0x00];
    const apic = [0, ...enc.encode("image/png"), 0, 3, 0, 1, 2, 3];
    const b = id3(4, [frame("TIT2", [1, ...utf16], 4), frame("APIC", apic, 4)]);
    const tags = parseId3(b);
    expect(tags?.title).toBe("Hi");
    expect(tags?.cover?.mime).toBe("image/png");
    expect([...(tags?.cover?.bytes ?? [])]).toEqual([1, 2, 3]);
  });
  it("returns null without an ID3 header, and stops at a broken frame", () => {
    expect(parseId3(new Uint8Array([1, 2, 3]))).toBeNull();
    const b = id3(3, [frame("TIT2", [3, ...enc.encode("A")], 3)]);
    b[10 + 7] = 0x7f; // inflate the frame size past the tag end
    expect(parseId3(b)).toEqual({});
  });
  it("reads MP4 ilst items", () => {
    const item = (name: number[], text: string): number[] => {
      const payload = [...enc.encode(text)];
      const data = [...be32(16 + payload.length), ...enc.encode("data"), ...be32(1), ...be32(0), ...payload];
      return [...be32(8 + data.length), ...name, ...data];
    };
    const nam = item([0xa9, ...enc.encode("nam")], "Track");
    const art = item([0xa9, ...enc.encode("ART")], "Band");
    const ilst = [...be32(8 + nam.length + art.length), ...enc.encode("ilst"), ...nam, ...art];
    expect(parseMp4Tags(new Uint8Array(ilst))).toEqual({ title: "Track", artist: "Band" });
  });
  it("titles by file name and bounds tags", () => {
    expect(titleFromFileName("01 Intro.mp3")).toBe("01 Intro");
    expect(boundTag("\u0001 x ")).toBe("x");
    expect(boundTag("   ")).toBeUndefined();
    expect(new TextEncoder().encode(boundTag("é".repeat(200)) ?? "").length).toBeLessThanOrEqual(255);
  });
  it("never throws and falls back to the file name", async () => {
    const tags = await readAudioTags(new File([new Uint8Array([0, 1, 2])], "Mix.m4a", { type: "audio/mp4" }));
    expect(tags.title).toBe("Mix");
    expect(tags.artist).toBeUndefined();
  });
});
