// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The `voices` line of the call's console diagnostics: for each person where
 * their sound level was read from, how loud they were and whether that
 * counted as speaking; who holds the floor; who is on the large tile and why.
 *
 * Written so the next live call answers what the last one could not: a
 * person who speaks and never reaches the large tile is either not measured
 * (`none`), measured below the threshold (a peak under `thr`, `no`), or
 * measured and outranked (a pin, or a floor someone else holds).
 *
 * One person reads, for example,
 *
 *     a=analyser:0.031:yes[thr=0.012,an=0.031,en=0.040,lv=0.120,src=0.030]
 *
 * which is: the path the ring was decided from, the loudest level on it over
 * the interval, whether the ring was on; then the threshold in force and the
 * raw value of every path that was tried (`audio-level.ts`): `an` the
 * analyser on the played stream (`!` after it: passed over as stuck at
 * zero), `en` the statistics energy level, `lv` the statistics' own
 * `audioLevel` and `src` the level on the receiver's sources. `lv` and `src`
 * never decide anything; they are there to be compared. A path with nothing
 * is a dash, and `en` then says why (`en=-(no-energy)`). This device's own
 * microphone has only `an`.
 *
 * Diagnostics only, on this device only; it decides nothing. Pure.
 */

import type { LevelPath, ReceiverDiagnostics } from "./audio-level";
import type { GroupLayoutKind, StageSpeech } from "./group-call-layout";

/** One person over one interval. */
export interface VoiceReport {
  readonly id: string;
  readonly isLocal: boolean;
  /** Where the last level of the interval came from. */
  readonly path: LevelPath;
  /** The loudest level of the interval, `0..1`; `null` when none was read. */
  readonly peak: number | null;
  /** Whether the person counted as speaking at any reading of the interval. */
  readonly speaking: boolean;
  /** The level that counts as sound for this person at the end of the
   *  interval. */
  readonly threshold: number;
  /** The loudest analyser reading of the interval; `null` when the analyser
   *  could not be read. */
  readonly analyser: number | null;
  /** The analyser was passed over as stuck at zero during the interval. */
  readonly analyserStuck: boolean;
  /** The raw values of the receiver's paths; `null` when the person has no
   *  receiver (this device's own microphone, or audio not yet arrived). */
  readonly receiver: ReceiverDiagnostics | null;
}

/** Why a person is on the large tile; the order of the shared rule. */
export type StageReason = "pinned" | "speaking" | "recent" | "picture" | "first";

/** The short name of a person in the line: the SFU peer id, `me` for this
 *  device. Never a display name. */
export function voiceLabel(id: string, isLocal: boolean): string {
  return isLocal ? "me" : id.replace(/^peer-/, "");
}

/**
 * Why `stageId` is on the large tile, given what the shared rule was asked
 * with. Mirrors the rule's order; it explains an answer, it does not make one.
 */
export function stageReason(
  stageId: string,
  people: readonly { readonly id: string; readonly camOn: boolean }[],
  pinnedId: string | null,
  speech: StageSpeech,
): StageReason {
  if (pinnedId === stageId) return "pinned";
  if (speech.activeSpeakerId === stageId) return "speaking";
  if ((speech.lastSpokeAtMs[stageId] ?? 0) > 0) return "recent";
  if (people.some((p) => p.id === stageId && p.camOn)) return "picture";
  return "first";
}

/** A level in the line: three decimals, since speech on a quiet microphone
 *  sits at a few hundredths; nothing read is a dash. */
function amount(level: number | null): string {
  return level === null ? "-" : level.toFixed(3);
}

/** The bracket of one person: the threshold and every path tried. */
export function describePaths(voice: VoiceReport): string {
  const parts = [
    `thr=${amount(voice.threshold)}`,
    `an=${amount(voice.analyser)}${voice.analyserStuck ? "!" : ""}`,
  ];
  const receiver = voice.receiver;
  if (receiver !== null) {
    parts.push(
      receiver.energy === null
        ? `en=-(${receiver.statsNote})`
        : `en=${amount(receiver.energy)}`,
      `lv=${amount(receiver.statsLevel)}`,
      `src=${amount(receiver.sources)}`,
    );
  }
  return `[${parts.join(",")}]`;
}

/** The line. `stage` is `null` when no large tile is drawn. */
export function describeVoices(
  prefix: string,
  voices: readonly VoiceReport[],
  floorId: string | null,
  stage: { readonly id: string; readonly reason: StageReason } | null,
  layout: GroupLayoutKind,
): string {
  const label = (id: string): string =>
    voiceLabel(id, voices.find((v) => v.id === id)?.isLocal === true);
  const people = voices.map((v) => {
    const head = `${voiceLabel(v.id, v.isLocal)}=${v.path}:${amount(v.peak)}:${v.speaking ? "yes" : "no"}`;
    return `${head}${describePaths(v)}`;
  });
  return [
    `${prefix} voices`,
    ...people,
    `floor=${floorId === null ? "-" : label(floorId)}`,
    `stage=${stage === null ? "-" : `${label(stage.id)}(${stage.reason})`}`,
    `layout=${layout}`,
  ].join(" ");
}
