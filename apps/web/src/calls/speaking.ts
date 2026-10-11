// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Who is speaking in a group call (`docs/ux/group-call-grid.md` sections 5.2
 * and 7.3).
 *
 * Worked out on this device from audio it has already received, decrypted and
 * decoded, so the call server never learns it. Two outputs with two different
 * timings, because they answer two different questions:
 *
 *  - **the ring** on a tile: on after 200 ms of sound, off after 800 ms of
 *    silence. It may come on for a cough.
 *  - **the floor**: the one person the large tile follows. A person takes it
 *    after 1.5 s of continuous speech, and keeps it while they keep speaking,
 *    so a cough never moves a tile and two people talking over each other do
 *    not make the stage flip between them.
 *
 * Every number is chosen, not measured.
 *
 * Pure and clock-free: the caller supplies the level and the time, so the
 * whole rule runs in a test.
 */

/** A level at or above this counts as sound. `0..1`, root mean square of the
 *  samples; ordinary speech into a laptop microphone sits well above it and a
 *  quiet room well below. */
export const SPEAKING_LEVEL = 0.02;
/** Sound for this long turns the ring on. */
export const RING_ON_MS = 200;
/** Silence for this long turns it off. */
export const RING_OFF_MS = 800;
/** Continuous speech for this long takes the floor. */
export const FLOOR_ONSET_MS = 1_500;

interface Voice {
  /** Since when the level has been above the threshold, or `null`. */
  soundSince: number | null;
  /** Since when it has been below, or `null`. */
  silenceSince: number | null;
  /** The ring. */
  speaking: boolean;
  /** Since when the ring has been on without a break, or `null`. */
  speakingSince: number | null;
  /** The last reading with sound while the ring was on, or `null`. */
  lastSoundAt: number | null;
}

/** What the tracker reports after a sample. */
export interface SpeakingSnapshot {
  /** Everyone whose ring is on. */
  readonly speakingIds: ReadonlySet<string>;
  /** The one person holding the floor, or `null`. */
  readonly activeSpeakerId: string | null;
  /** When each person last held the floor. */
  readonly lastSpokeAtMs: Readonly<Record<string, number>>;
}

/** Follows the people of one call. */
export class SpeakingTracker {
  private readonly voices = new Map<string, Voice>();
  private floor: string | null = null;
  private lastSpoke: Record<string, number> = {};

  /** One level reading for one person. */
  public sample(id: string, level: number, nowMs: number): void {
    let v = this.voices.get(id);
    if (v === undefined) {
      v = { soundSince: null, silenceSince: null, speaking: false, speakingSince: null, lastSoundAt: null };
      this.voices.set(id, v);
    }
    if (Number.isFinite(level) && level >= SPEAKING_LEVEL) {
      v.silenceSince = null;
      v.soundSince ??= nowMs;
      if (!v.speaking && nowMs - v.soundSince >= RING_ON_MS) {
        v.speaking = true;
        v.speakingSince = v.soundSince;
      }
      if (v.speaking) v.lastSoundAt = nowMs;
    } else {
      v.soundSince = null;
      v.silenceSince ??= nowMs;
      if (v.speaking && nowMs - v.silenceSince >= RING_OFF_MS) {
        v.speaking = false;
        v.speakingSince = null;
        v.lastSoundAt = null;
      }
    }
  }

  /**
   * The person is muted: their ring goes off at once and they give up the
   * floor at the next snapshot. A muted person is never speaking, whatever a
   * level reading says.
   */
  public silence(id: string): void {
    const v = this.voices.get(id);
    if (v === undefined) return;
    v.soundSince = null;
    v.silenceSince = null;
    v.speaking = false;
    v.speakingSince = null;
    v.lastSoundAt = null;
  }

  /** Forget people who are no longer in the call. */
  public keepOnly(ids: ReadonlySet<string>): void {
    for (const id of [...this.voices.keys()]) {
      if (!ids.has(id)) this.voices.delete(id);
    }
    if (this.floor !== null && !ids.has(this.floor)) this.floor = null;
    this.lastSpoke = Object.fromEntries(
      Object.entries(this.lastSpoke).filter(([id]) => ids.has(id)),
    );
  }

  /**
   * The state after the samples so far.
   *
   * `candidates` are the people who can hold the floor, in roster order,
   * which breaks the tie when two reach the onset in the same reading. This
   * device's own person is NOT one of them: the floor exists for the large
   * tile, which never shows the own tile while anyone else is present, and a
   * floor held by the own microphone (the person talking, or the room heard
   * through it) kept everyone else from taking it, so the large tile never
   * moved to the person speaking. The ring is for everyone sampled, the own
   * tile included.
   */
  public snapshot(candidates: readonly string[], nowMs: number): SpeakingSnapshot {
    const speakingIds = new Set<string>();
    for (const [id, v] of this.voices) if (v.speaking) speakingIds.add(id);

    /** The ring is on: enough to keep a floor already held. */
    const stillSpeaking = (id: string): boolean => this.voices.get(id)?.speaking === true;
    /**
     * Enough to take the floor: the speech itself has lasted the onset. The
     * time the ring stays on after the last sound does not count, or a
     * 0.8-second sound would grow into a 1.5-second one by standing still.
     */
    const earned = (id: string): boolean => {
      const v = this.voices.get(id);
      return (
        v !== undefined &&
        v.speaking &&
        v.speakingSince !== null &&
        v.lastSoundAt !== null &&
        v.lastSoundAt - v.speakingSince >= FLOOR_ONSET_MS
      );
    };
    // Whoever holds the floor keeps it while they keep speaking; otherwise it
    // goes to the first candidate in roster order who has earned it. When the
    // holder stops and nobody has, the floor is empty and `lastSpokeAtMs`
    // still names them, which is what keeps the large tile on them.
    if (this.floor === null || !candidates.includes(this.floor) || !stillSpeaking(this.floor)) {
      this.floor = candidates.find(earned) ?? null;
    }
    if (this.floor !== null) {
      this.lastSpoke = { ...this.lastSpoke, [this.floor]: nowMs };
    }
    return {
      speakingIds,
      activeSpeakerId: this.floor,
      lastSpokeAtMs: this.lastSpoke,
    };
  }
}

/** Root mean square of a block of samples in `-1..1`. */
export function rmsLevel(samples: Float32Array): number {
  if (samples.length === 0) return 0;
  let sum = 0;
  for (const s of samples) sum += s * s;
  return Math.sqrt(sum / samples.length);
}
