// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * What the SFU actually gives this device of each person's video, and the
 * picture a tile therefore shows (ADR-0070 step 6).
 *
 * This device tells the SFU what each tile wants (`video-wants.ts`). With
 * downlink control on, the SFU may give less when this device's downlink is
 * short, and says so in a `video-given` frame: the whole set each time, one
 * entry per person it sends video of. A track it does not give is paused on
 * purpose and resumed at a key frame.
 *
 * The rule here:
 *
 *   * the picture a tile shows is the **lower** of what the tile wants and
 *     what the SFU gives ({@link effectiveQuality});
 *   * no `video-given` ever received, or a person the latest one does not
 *     name: what the tile wants, so an SFU with downlink control off behaves
 *     exactly as before this module;
 *   * a new frame replaces the whole previous set; a person who leaves is
 *     forgotten ({@link VideoGivenBook});
 *   * what this device **asks** for never changes because of a `video-given`:
 *     the SFU must keep knowing the real wish to be able to give it back.
 *     Nothing in this module feeds `VideoWantsBook`.
 *
 * Pure: no DOM, no Solid, no timers.
 */

import {
  askedQuality,
  heavyStreamFor,
  isVideoQuality,
  lowerQuality,
  type VideoQuality,
} from "./video-wants";

/** One entry of a `video-given` frame. */
export interface VideoGivenEntry {
  readonly peer: string;
  readonly quality: VideoQuality;
}

/**
 * The flat list the wasm helper returns (`parseSfuVideoGiven`: peer0,
 * quality0, peer1, ...) as entries, in the frame's order. `null` when the
 * list is not pairs, a peer id is empty, or a quality is not one of the
 * three names: the frame is then ignored as a whole.
 */
export function videoGivenFromPairs(pairs: readonly string[]): VideoGivenEntry[] | null {
  if (pairs.length % 2 !== 0) return null;
  const out: VideoGivenEntry[] = [];
  for (let i = 0; i < pairs.length; i += 2) {
    const peer = pairs[i];
    const quality = pairs[i + 1];
    if (typeof peer !== "string" || peer.length === 0 || !isVideoQuality(quality)) return null;
    out.push({ peer, quality });
  }
  return out;
}

/** The wasm helper that validates a `video-given` frame against the
 *  protocol's bounds and flattens it. Structural, so a test can hand in the
 *  real wasm namespace or a stand-in. */
export interface VideoGivenParser {
  parseSfuVideoGiven(json: string): string[];
}

/**
 * One inbound `video-given` text frame as entries, or `null` when it is
 * malformed (not such a frame, outside the protocol's bounds, an unknown
 * quality). Never throws.
 */
export function readVideoGivenFrame(parser: VideoGivenParser, raw: string): VideoGivenEntry[] | null {
  let pairs: string[];
  try {
    pairs = parser.parseSfuVideoGiven(raw);
  } catch {
    return null;
  }
  return videoGivenFromPairs(pairs);
}

/**
 * Whether a text frame the shared validator refused says it is a
 * `video-given` one. Only decides whether the refusal is worth a warning;
 * nothing is read from such a frame.
 */
export function claimsVideoGiven(raw: string): boolean {
  try {
    const parsed: unknown = JSON.parse(raw);
    return (
      typeof parsed === "object" &&
      parsed !== null &&
      (parsed as { kind?: unknown }).kind === "video-given"
    );
  } catch {
    return false;
  }
}

/** The one warning a malformed `video-given` frame gets. */
export const VIDEO_GIVEN_MALFORMED_WARNING = "sfu signaling: ignored a malformed video-given frame";

/**
 * The picture a tile shows: the lower of what it wants and what the SFU
 * gives. `given` is `undefined` (or `null`) when the SFU has said nothing
 * about the person; the tile then shows what it wants.
 */
export function effectiveQuality(
  wanted: VideoQuality,
  given: VideoQuality | null | undefined,
): VideoQuality {
  return given === null || given === undefined ? wanted : lowerQuality(wanted, given);
}

/**
 * {@link effectiveQuality} as the quality the tile's elements are built for.
 * `light` means the light stream alone, which is nothing at all for a person
 * who sends no light copy: such a tile keeps the heavy picture it wants (as
 * `askedQuality` has it), paused or not, because there is nothing else to
 * show.
 */
export function shownQuality(
  wanted: VideoQuality,
  given: VideoQuality | null | undefined,
  hasLight: boolean,
): VideoQuality {
  const effective = effectiveQuality(wanted, given);
  return effective === "light" && !hasLight ? askedQuality(wanted, false) : effective;
}

/**
 * The stream a tile's heavy `<video>` plays, given what the tile wants and
 * what the SFU gives: the main stream at `main`; the middle stream at
 * `middle` (the main one while the person has no middle track); none at
 * `light`, where the light stream is shown alone and no heavy element waits
 * for frames the SFU is not sending.
 */
export function heavyStreamForGiven<S>(
  wanted: VideoQuality,
  given: VideoQuality | null | undefined,
  main: S | undefined,
  middle: S | undefined,
  hasLight: boolean,
): S | undefined {
  return heavyStreamFor(shownQuality(wanted, given, hasLight), main, middle);
}

/**
 * The latest `video-given` set, by person.
 *
 * Empty until the first frame. Each frame replaces the whole set, so a person
 * an earlier frame named and the latest does not is back to "nothing said".
 */
export class VideoGivenBook {
  private given = new Map<string, VideoQuality>();

  /** A `video-given` frame arrived: its entries are the whole set now. A
   *  person named twice has the later entry. */
  replace(entries: Iterable<VideoGivenEntry>): void {
    const next = new Map<string, VideoQuality>();
    for (const e of entries) next.set(e.peer, e.quality);
    this.given = next;
  }

  /** A person left. Returns whether they had an entry. */
  removePeer(peerId: string): boolean {
    return this.given.delete(peerId);
  }

  /** What the SFU gives of `peerId`; `undefined` when it has not said. */
  givenOf(peerId: string): VideoQuality | undefined {
    return this.given.get(peerId);
  }

  /** How many people the set names. */
  size(): number {
    return this.given.size;
  }

  /** The set as it stands, a copy. */
  snapshot(): ReadonlyMap<string, VideoQuality> {
    return new Map(this.given);
  }

  /** Forget everything: the call is over. */
  clear(): void {
    this.given = new Map();
  }
}

/** One person whose shown picture a `video-given` frame changed. */
export interface VideoGivenChange {
  readonly peer: string;
  readonly wanted: VideoQuality;
  /** What the SFU gives now; `null` when the frame does not name them. */
  readonly given: VideoQuality | null;
}

/**
 * The people whose effective quality differs between two sets, in the order
 * of `wants` (what this device asks of each person now).
 */
export function videoGivenChanges(
  wants: ReadonlyMap<string, VideoQuality>,
  before: (peerId: string) => VideoQuality | undefined,
  after: (peerId: string) => VideoQuality | undefined,
): VideoGivenChange[] {
  const out: VideoGivenChange[] = [];
  for (const [peer, wanted] of wants) {
    const now = after(peer);
    if (effectiveQuality(wanted, before(peer)) !== effectiveQuality(wanted, now)) {
      out.push({ peer, wanted, given: now ?? null });
    }
  }
  return out;
}
