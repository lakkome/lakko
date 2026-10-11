// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The `voices` line of the call's console diagnostics: for each person where
 * their sound level was read from, how loud they were and whether that
 * counted as speaking; who holds the floor; who is on the large tile and why.
 *
 * Written so the next live call answers what the last one could not: a
 * person who speaks and never reaches the large tile is either not measured
 * (`none`), measured below the threshold (a small peak, `no`), or measured
 * and outranked (a pin, or a floor someone else holds).
 *
 * Diagnostics only, on this device only; it decides nothing. Pure.
 */

import type { LevelPath } from "./audio-level";
import type { GroupLayoutKind, StageSpeech } from "./group-call-layout";

/** One person over one interval. */
export interface VoiceReport {
  readonly id: string;
  readonly isLocal: boolean;
  /** Where the last level of the interval came from. */
  readonly path: LevelPath | "analyser";
  /** The loudest level of the interval, `0..1`; `null` when none was read. */
  readonly peak: number | null;
  /** Whether the person counted as speaking at any reading of the interval. */
  readonly speaking: boolean;
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

const PATH_NAMES: Record<VoiceReport["path"], string> = {
  "sync-source": "sources",
  "stats-energy": "stats-energy",
  "stats-level": "stats-level",
  analyser: "analyser",
  none: "none",
};

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
    const peak = v.peak === null ? "-" : v.peak.toFixed(2);
    return `${voiceLabel(v.id, v.isLocal)}=${PATH_NAMES[v.path]}:${peak}:${v.speaking ? "yes" : "no"}`;
  });
  return [
    `${prefix} voices`,
    ...people,
    `floor=${floorId === null ? "-" : label(floorId)}`,
    `stage=${stage === null ? "-" : `${label(stage.id)}(${stage.reason})`}`,
    `layout=${layout}`,
  ].join(" ");
}
