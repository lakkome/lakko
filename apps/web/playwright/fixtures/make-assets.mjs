// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The two audio assets the real-browser specs need, generated rather than
 * committed.
 *
 * Neither is a decoration. A voice bubble whose element has nothing decodable
 * never reaches `playing`, so the height measurement would compare a bubble
 * before a press with the same bubble after a press that did nothing — green
 * for the wrong reason. And Chromium's fake microphone reads a real file off
 * disk; without one it produces silence, which is indistinguishable from a
 * recorder that captured nothing.
 *
 *   * `voice.m4a` — AAC in MP4, the one format ADR-0047 allows for a voice
 *     message, so the fixture hands the production hub exactly the bytes and
 *     the MIME a real sender would. Needs `ffmpeg`.
 *   * `avatar.mp4` — one second of H.264 in MP4, no audio, square: the shape
 *     of an ADR-0084 profile video, for the avatar hover spec. Needs `ffmpeg`;
 *     without it that spec is skipped by name.
 *   * `mic.wav` — 16-bit PCM, for `--use-file-for-fake-audio-capture`. Written
 *     by hand below: a WAV header is 44 bytes and a dependency is not worth it.
 *     Chromium loops it, and every sample is loud, so any window a recorder
 *     captures is non-silent — which is what makes a silent or truncated
 *     capture a failure rather than a coin toss.
 *
 * `ffmpeg` is NOT installed by this script. When it is absent the voice asset
 * is written as a WAV instead and the specs that need a decodable voice
 * message are skipped by name, because the alternative — carrying on with an
 * asset the browser may refuse — turns a missing tool into a false green.
 */

import { execFileSync } from "node:child_process";
import { existsSync, mkdirSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const HERE = dirname(fileURLToPath(import.meta.url));

export const VOICE_M4A = join(HERE, "voice.m4a");
export const VOICE_WAV = join(HERE, "voice.wav");
export const MIC_WAV = join(HERE, "mic.wav");
export const AVATAR_MP4 = join(HERE, "avatar.mp4");

const SAMPLE_RATE = 48000;

/** A 16-bit mono PCM WAV of `seconds` at `hz`, full-ish amplitude. */
function wav(seconds, hz) {
  const frames = Math.round(SAMPLE_RATE * seconds);
  const data = Buffer.alloc(frames * 2);
  for (let i = 0; i < frames; i += 1) {
    // 0.6 of full scale: loud enough that no level gate reads it as silence,
    // short of clipping so a decoder has nothing to complain about.
    const s = Math.sin((2 * Math.PI * hz * i) / SAMPLE_RATE) * 0.6;
    data.writeInt16LE(Math.round(s * 32767), i * 2);
  }
  const header = Buffer.alloc(44);
  header.write("RIFF", 0, "ascii");
  header.writeUInt32LE(36 + data.length, 4);
  header.write("WAVE", 8, "ascii");
  header.write("fmt ", 12, "ascii");
  header.writeUInt32LE(16, 16); // PCM fmt chunk size
  header.writeUInt16LE(1, 20); // PCM
  header.writeUInt16LE(1, 22); // mono
  header.writeUInt32LE(SAMPLE_RATE, 24);
  header.writeUInt32LE(SAMPLE_RATE * 2, 28); // byte rate
  header.writeUInt16LE(2, 32); // block align
  header.writeUInt16LE(16, 34); // bits
  header.write("data", 36, "ascii");
  header.writeUInt32LE(data.length, 40);
  return Buffer.concat([header, data]);
}

function hasFfmpeg() {
  try {
    execFileSync("ffmpeg", ["-version"], { stdio: "ignore" });
    return true;
  } catch {
    return false;
  }
}

/**
 * Write whatever is missing and report what the voice asset ended up being.
 * Idempotent: the specs' global setup calls this on every run.
 */
export function makeAssets({ force = false } = {}) {
  mkdirSync(HERE, { recursive: true });

  if (force || !existsSync(MIC_WAV)) {
    // Ten seconds, so a short take never wraps the loop point mid-window.
    writeFileSync(MIC_WAV, wav(10, 440));
  }

  if ((force || !existsSync(AVATAR_MP4)) && hasFfmpeg()) {
    execFileSync(
      "ffmpeg",
      ["-y", "-loglevel", "error", "-f", "lavfi", "-i", "testsrc=size=96x96:rate=15:duration=1",
       "-c:v", "libx264", "-pix_fmt", "yuv420p", "-an", "-movflags", "+faststart", AVATAR_MP4],
      { stdio: "inherit" },
    );
  }

  if (!force && existsSync(VOICE_M4A)) return { voice: VOICE_M4A, kind: "m4a" };

  if (hasFfmpeg()) {
    const src = wav(7, 330);
    writeFileSync(VOICE_WAV, src);
    execFileSync(
      "ffmpeg",
      // `-y` overwrite, AAC-LC in MP4, `+faststart` so the moov atom is at the
      // front — a blob URL is served whole, but a front-loaded file is what a
      // real sender's take looks like.
      ["-y", "-loglevel", "error", "-i", VOICE_WAV, "-c:a", "aac", "-b:a", "64k",
       "-movflags", "+faststart", VOICE_M4A],
      { stdio: "inherit" },
    );
    return { voice: VOICE_M4A, kind: "m4a" };
  }

  if (force || !existsSync(VOICE_WAV)) writeFileSync(VOICE_WAV, wav(7, 330));
  return { voice: VOICE_WAV, kind: "wav" };
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const out = makeAssets({ force: process.argv.includes("--force") });
  process.stdout.write(`voice asset: ${out.voice} (${out.kind})\nmic asset: ${MIC_WAV}\n`);
}
