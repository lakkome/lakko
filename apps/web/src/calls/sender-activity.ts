// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Which of this device's camera senders encode and send now (ADR-0070,
 * "senders nobody watches").
 *
 * The SFU tells a publisher which of its own camera senders somebody is being
 * sent: `{"kind":"video-needed","main":bool,"middle":bool}`, the whole state
 * each time, the last frame wins. The light sender is always needed and is
 * not named. This module holds
 *
 *   * the reading of that frame ({@link readVideoNeededFrame});
 *   * the one decision ({@link senderActive}): a sender's encoding is active
 *     exactly when the camera is on, the sender exists, the people-count rule
 *     allows it (the middle sender only), and the SFU's last word for it is
 *     not `false`. No word yet means needed. The light sender ignores the
 *     SFU's word;
 *   * its application to one `RTCRtpSender` ({@link SenderActivity}), with
 *     `setParameters` on `encodings[0].active` and nothing else.
 *
 * Why the encoding's `active` flag and neither of the two other ways to
 * silence a sender: a track with `enabled = false` is still encoded and sent
 * (black frames), so nothing is saved; and `replaceTrack(null)` takes the
 * source away from the transceiver, which on some engines changes what the
 * media line negotiates at the next offer. The flag stops the encoder and the
 * packets and leaves the transceiver, the m-line, the SSRC and the SFrame
 * transform, key and stream index as they are: no renegotiation.
 *
 * Pure apart from {@link SenderActivity}, which touches only the sender it is
 * handed: no DOM, no Solid, no timers.
 */

/** One of this device's camera senders. */
export type CameraSenderKind = "main" | "middle" | "light";

/** Every camera sender, in the order the diagnostics name them. */
export const CAMERA_SENDER_KINDS: readonly CameraSenderKind[] = ["main", "middle", "light"];

/** The SFU's last `video-needed` frame. */
export interface VideoNeeded {
  readonly main: boolean;
  readonly middle: boolean;
}

/** The wasm helper that validates a `video-needed` frame and returns its two
 *  flags as bytes `[main, middle]`, each 1 or 0. Structural, so a test can
 *  hand in the real wasm namespace or a stand-in. */
export interface VideoNeededParser {
  parseSfuVideoNeeded(json: string): Uint8Array;
}

/**
 * One inbound `video-needed` text frame, or `null` when it is malformed (not
 * such a frame, a flag missing or of another type, or a helper that returns
 * anything but exactly two bytes of 0 or 1). Never throws.
 */
export function readVideoNeededFrame(parser: VideoNeededParser, raw: string): VideoNeeded | null {
  let flags: Uint8Array;
  try {
    flags = parser.parseSfuVideoNeeded(raw);
  } catch {
    return null;
  }
  if (flags.length !== 2) return null;
  const main = flags[0];
  const middle = flags[1];
  if ((main !== 0 && main !== 1) || (middle !== 0 && middle !== 1)) return null;
  return { main: main === 1, middle: middle === 1 };
}

/**
 * Whether a text frame the shared validator refused says it is a
 * `video-needed` one. Only decides whether the refusal is worth a warning;
 * nothing is read from such a frame.
 */
export function claimsVideoNeeded(raw: string): boolean {
  try {
    const parsed: unknown = JSON.parse(raw);
    return (
      typeof parsed === "object" &&
      parsed !== null &&
      (parsed as { kind?: unknown }).kind === "video-needed"
    );
  } catch {
    return false;
  }
}

/** The one warning a malformed `video-needed` frame gets. */
export const VIDEO_NEEDED_MALFORMED_WARNING = "sfu signaling: ignored a malformed video-needed frame";

/**
 * The SFU's last word for one sender: `true` or `false` for the main and the
 * middle sender once a frame arrived, `null` before any frame and always for
 * the light sender, which the frame does not name.
 */
export function neededWord(kind: CameraSenderKind, needed: VideoNeeded | null): boolean | null {
  if (needed === null || kind === "light") return null;
  return kind === "main" ? needed.main : needed.middle;
}

/** What {@link senderActive} decides from. */
export interface SenderActivityInput {
  /** The person's camera is on. */
  readonly cameraOn: boolean;
  /** This device has that sender. */
  readonly exists: boolean;
  /** The people-count rule (`middle_sender_wanted`) says the middle sender
   *  is sent. Read for the middle sender only. */
  readonly middleWanted: boolean;
  /** The SFU's last `video-needed` frame; `null` before the first. */
  readonly needed: VideoNeeded | null;
}

/**
 * Whether `kind`'s encoding is active: every condition has to hold.
 *
 *   * the camera is on;
 *   * the sender exists;
 *   * for the middle sender, the people-count rule;
 *   * the SFU's last word for it is not `false` (no word yet: needed). The
 *     light sender has no word and is never switched off by the SFU.
 */
export function senderActive(kind: CameraSenderKind, input: SenderActivityInput): boolean {
  if (!input.cameraOn || !input.exists) return false;
  if (kind === "middle" && !input.middleWanted) return false;
  return neededWord(kind, input.needed) !== false;
}

/** {@link senderActive} for every sender. */
export function sendersActive(
  input: Omit<SenderActivityInput, "exists">,
  exists: (kind: CameraSenderKind) => boolean,
): Record<CameraSenderKind, boolean> {
  const of = (kind: CameraSenderKind): boolean => senderActive(kind, { ...input, exists: exists(kind) });
  return { main: of("main"), middle: of("middle"), light: of("light") };
}

/** A sender's `needed=` field in the diagnostics: `yes`, `no`, or `-` when
 *  the SFU has said nothing about it (the light sender always). */
export function neededField(word: boolean | null): "yes" | "no" | "-" {
  return word === null ? "-" : word ? "yes" : "no";
}

/** What the diagnostics say of one sender. */
export interface SenderActivityNote {
  /** The SFU's last word; `null` when it has said nothing. */
  readonly needed: boolean | null;
  /** Whether the sender's encoding is asked to be active now. */
  readonly active: boolean;
}

/** ` needed=yes|no|- active=yes|no`, the tail a `send video` line gains. */
export function describeSenderActivity(note: SenderActivityNote): string {
  return ` needed=${neededField(note.needed)} active=${note.active ? "yes" : "no"}`;
}

/**
 * The one line for a `video-needed` frame that changed which sender is
 * active: `call video needed main=.. middle=..` with what is active now.
 * `null` when the frame changed nothing that is active.
 */
export function describeVideoNeededChange(
  needed: VideoNeeded,
  before: Readonly<Record<CameraSenderKind, boolean>>,
  after: Readonly<Record<CameraSenderKind, boolean>>,
): string | null {
  if (CAMERA_SENDER_KINDS.every((kind) => before[kind] === after[kind])) return null;
  const yn = (b: boolean): string => (b ? "yes" : "no");
  return (
    `call video needed main=${yn(needed.main)} middle=${yn(needed.middle)}` +
    ` active main=${yn(after.main)} middle=${yn(after.middle)} light=${yn(after.light)}`
  );
}

/** The parts of an `RTCRtpSender` {@link SenderActivity} uses. */
export interface ActivitySender {
  getParameters(): RTCRtpSendParameters;
  setParameters(parameters: RTCRtpSendParameters): Promise<unknown>;
}

/** What one application did. */
export type ActivityOutcome =
  /** The encoding says what was wanted (already, or after `setParameters`). */
  | "applied"
  /** The sender has no encoding to set the flag on. */
  | "no-encoding"
  /** The browser refused. */
  | "refused";

/**
 * Keeps one sender's `encodings[0].active` at what was last asked.
 *
 *   * **Serialised.** `setParameters` is asynchronous and a browser rejects
 *     one made on a `getParameters` reading that another call has outdated.
 *     Each step reads, modifies and sets in one go, and the next step starts
 *     only when it has settled.
 *   * **The last word wins.** A step applies what is wanted when it runs, so
 *     `true, false, true` asked faster than the browser answers ends at
 *     `true`, in at most two `setParameters` calls, never out of order.
 *   * **Re-applied.** Every step reads the encoding as the browser holds it
 *     now, so asking again after a renegotiation, a replaced track or a new
 *     sender ({@link bind}) puts back a flag the browser reset.
 *   * **When in doubt, active.** A refused switch-off leaves the sender
 *     sending and is not asked again until something else is wanted. A
 *     refused switch-on is asked again at every later {@link want}, because a
 *     sender somebody watches must not stay off. Either is said once per
 *     sender.
 */
export class SenderActivity {
  private sender: ActivitySender | null = null;
  private wanted: boolean | null = null;
  private scheduled = false;
  private chain: Promise<void> = Promise.resolve();
  /** A switch-off the browser refused on this sender: not asked again while
   *  switch-off stays what is wanted. */
  private offRefused = false;
  private warned = false;

  constructor(
    /** Names the sender in the one warning: `main`, `middle` or `light`. */
    private readonly name: string,
    private readonly warn: (message: string, err: unknown) => void = (message, err) => {
      // eslint-disable-next-line no-console
      console.warn(message, err ?? "");
    },
  ) {}

  /**
   * The sender this keeps. A different one (first publish, mid-call
   * camera-on, a copy added again) starts from nothing: no refusal is
   * remembered and its warning may be said. `null`: there is none.
   */
  bind(sender: ActivitySender | null): void {
    if (this.sender === sender) return;
    this.sender = sender;
    this.offRefused = false;
    this.warned = false;
  }

  /** Whether {@link bind} was given a sender. */
  get bound(): boolean {
    return this.sender !== null;
  }

  /** What was last asked; `null` before anything was. */
  get lastWanted(): boolean | null {
    return this.wanted;
  }

  /**
   * Ask for the encoding to be active or not. Safe to call at any time and
   * as often as wanted: it costs one synchronous `getParameters` when the
   * encoding already says so.
   */
  want(active: boolean): void {
    if (this.wanted !== active) this.offRefused = false;
    this.wanted = active;
    if (this.sender === null || this.scheduled) return;
    this.scheduled = true;
    this.chain = this.chain.then(() => this.step());
  }

  /** Resolves when every step asked for so far has settled. */
  settled(): Promise<void> {
    return this.chain;
  }

  /** Forget the sender and what was asked: the call is over. */
  reset(): void {
    this.sender = null;
    this.wanted = null;
    this.offRefused = false;
    this.warned = false;
  }

  private async step(): Promise<void> {
    this.scheduled = false;
    const sender = this.sender;
    const active = this.wanted;
    if (sender === null || active === null) return;
    if (!active && this.offRefused) return;
    const outcome = await applyEncodingActive(sender, active);
    // The sender was replaced while the browser answered: the new one is
    // asked by the caller's next `want`.
    if (this.sender !== sender) return;
    if (outcome.result !== "refused") return;
    if (!active) this.offRefused = true;
    if (this.warned) return;
    this.warned = true;
    this.warn(
      `camera sender activity: the ${this.name} camera sender could not be ${active ? "started" : "stopped"}` +
        (active ? ", asking again" : ", it goes on sending"),
      outcome.error,
    );
  }
}

/**
 * One read-modify-set of `encodings[0].active`. Never throws. An encoding
 * that does not say is active, as every encoding is by default.
 */
export async function applyEncodingActive(
  sender: ActivitySender,
  active: boolean,
): Promise<{ result: ActivityOutcome; error?: unknown }> {
  try {
    const params = sender.getParameters();
    const first = params.encodings[0];
    if (first === undefined) return { result: "no-encoding" };
    if ((first.active ?? true) === active) return { result: "applied" };
    params.encodings[0] = { ...first, active };
    await sender.setParameters(params);
    return { result: "applied" };
  } catch (error) {
    return { result: "refused", error };
  }
}
