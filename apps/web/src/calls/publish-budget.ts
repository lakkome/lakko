// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * How much this device's camera senders may send, as the SFU says
 * (ADR-0068, amendment of 2026-10-11).
 *
 * The SFU estimates this device's uplink and tells it, over Lakko's own
 * signaling, the ceiling for its main and its middle camera sender:
 * `{"kind":"publish-budget","main_kbps":N,"middle_kbps":N}`, the whole state
 * each time, the last frame wins. The same estimate goes out as REMB, but a
 * browser's own bitrate controller decides what it makes of a REMB, and one
 * was measured not to follow it on its main sender while it did keep a
 * sender under a ceiling set with `setParameters`. So the ceiling is what
 * this module applies: `encodings[0].maxBitrate`, one call, the same in every
 * engine that has `setParameters`.
 *
 * The server decides, the client executes. There is no allocation rule here:
 * how the estimate is shared between the senders, the floors and the maxima
 * are the SFU's (`services/sfu/src/publish_budget.rs`), so the three clients
 * cannot drift. This module holds
 *
 *   * the reading of the frame ({@link readPublishBudgetFrame});
 *   * the one mapping ({@link senderCeiling}): the main sender's ceiling is
 *     `main_kbps`, the middle sender's is `middle_kbps`, in bits a second;
 *     the light sender is never touched (it keeps the ceiling it was created
 *     with); and before the first frame nothing is written, so each sender
 *     keeps what it was created with (the main one no ceiling, the middle one
 *     `CAMERA_MIDDLE_MAX_BITRATE`);
 *   * its application to one `RTCRtpSender` ({@link SenderCeiling}), with
 *     `setParameters` on `encodings[0].maxBitrate` and nothing else.
 *
 * A ceiling is applied to a sender whether or not it is switched on
 * (`sender-activity.ts`): the SFU gives a figure for a sender nobody watches
 * too, and it is what that sender starts under.
 *
 * Audio is never limited here.
 *
 * Pure apart from {@link SenderCeiling}, which touches only the sender it is
 * handed: no DOM, no Solid, no timers.
 */

import type { CameraSenderKind } from "./sender-activity";
import { queueParameterWrite } from "./sender-parameters";

/** The SFU's last `publish-budget` frame, kilobits a second. */
export interface PublishBudget {
  readonly mainKbps: number;
  readonly middleKbps: number;
}

/** The highest ceiling the frame may carry, kbit/s
 *  (`lakko_sfu_proto::MAX_PUBLISH_BUDGET_KBPS`). Checked by the shared
 *  parser; repeated here only to refuse a helper that returns nonsense. */
export const MAX_PUBLISH_BUDGET_KBPS = 20_000;

/** The wasm helper that validates a `publish-budget` frame and returns its
 *  two ceilings `[main_kbps, middle_kbps]`. Structural, so a test can hand in
 *  the real wasm namespace or a stand-in. */
export interface PublishBudgetParser {
  parseSfuPublishBudget(json: string): Uint32Array;
}

function validKbps(value: number | undefined): value is number {
  return value !== undefined && Number.isInteger(value) && value >= 1 && value <= MAX_PUBLISH_BUDGET_KBPS;
}

/**
 * One inbound `publish-budget` text frame, or `null` when it is malformed
 * (not such a frame, a ceiling missing, zero, above the protocol's bound or
 * not an integer, or a helper that returns anything but exactly two such
 * values). Never throws.
 */
export function readPublishBudgetFrame(parser: PublishBudgetParser, raw: string): PublishBudget | null {
  let ceilings: Uint32Array;
  try {
    ceilings = parser.parseSfuPublishBudget(raw);
  } catch {
    return null;
  }
  if (ceilings.length !== 2) return null;
  const mainKbps = ceilings[0];
  const middleKbps = ceilings[1];
  if (!validKbps(mainKbps) || !validKbps(middleKbps)) return null;
  return { mainKbps, middleKbps };
}

/**
 * Whether a text frame the shared validator refused says it is a
 * `publish-budget` one. Only decides whether the refusal is worth a warning;
 * nothing is read from such a frame.
 */
export function claimsPublishBudget(raw: string): boolean {
  try {
    const parsed: unknown = JSON.parse(raw);
    return (
      typeof parsed === "object" &&
      parsed !== null &&
      (parsed as { kind?: unknown }).kind === "publish-budget"
    );
  } catch {
    return false;
  }
}

/** The one warning a malformed `publish-budget` frame gets. */
export const PUBLISH_BUDGET_MALFORMED_WARNING = "sfu signaling: ignored a malformed publish-budget frame";

/** The senders the frame names. */
export type BudgetedSenderKind = Exclude<CameraSenderKind, "light">;

/** Every sender the frame names, in the order the diagnostics name them. */
export const BUDGETED_SENDER_KINDS: readonly BudgetedSenderKind[] = ["main", "middle"];

/**
 * The ceiling to set on `kind`'s encoding, bits a second, or `null` when
 * nothing is to be written: the light sender always, and every sender before
 * the first frame (it keeps what it was created with).
 */
export function senderCeiling(kind: CameraSenderKind, budget: PublishBudget | null): number | null {
  if (budget === null || kind === "light") return null;
  return (kind === "main" ? budget.mainKbps : budget.middleKbps) * 1000;
}

/** Whether two budgets say the same. */
export function sameBudget(a: PublishBudget | null, b: PublishBudget | null): boolean {
  if (a === null || b === null) return a === b;
  return a.mainKbps === b.mainKbps && a.middleKbps === b.middleKbps;
}

/**
 * The one line for a `publish-budget` frame that changed the budget: `call
 * publish budget main=.. middle=..`, kbit/s. `null` when the frame repeats
 * the last one.
 */
export function describePublishBudgetChange(
  before: PublishBudget | null,
  budget: PublishBudget,
): string | null {
  if (sameBudget(before, budget)) return null;
  return `call publish budget main=${budget.mainKbps} middle=${budget.middleKbps}`;
}

/**
 * ` ceiling=N`, the tail a `send video` line gains: the bitrate ceiling the
 * sender's encoding holds now, in kbit/s (rounded down), or `-` when it holds
 * none or the sender could not be read. `undefined`: say nothing.
 */
export function describeSenderCeiling(maxBitrate: number | null | undefined): string {
  if (maxBitrate === undefined) return "";
  return ` ceiling=${maxBitrate === null ? "-" : Math.floor(maxBitrate / 1000)}`;
}

/** The parts of an `RTCRtpSender` {@link SenderCeiling} uses. */
export interface CeilingSender {
  getParameters(): RTCRtpSendParameters;
  setParameters(parameters: RTCRtpSendParameters): Promise<unknown>;
}

/**
 * The ceiling `sender`'s one encoding holds now, bits a second; `null` when
 * it holds none, has no encoding, or cannot be read. Never throws.
 */
export function readEncodingCeiling(sender: Pick<CeilingSender, "getParameters">): number | null {
  try {
    const maxBitrate = sender.getParameters().encodings[0]?.maxBitrate;
    return typeof maxBitrate === "number" ? maxBitrate : null;
  } catch {
    return null;
  }
}

/** What one application did. */
export type CeilingOutcome =
  /** The encoding holds the ceiling (already, or after `setParameters`), or
   *  there was nothing to apply. */
  | "applied"
  /** The sender has no encoding to set the ceiling on. */
  | "no-encoding"
  /** The browser refused. */
  | "refused";

/**
 * One read-modify-set of `encodings[0].maxBitrate`. `wanted` is read when
 * the write runs, so the last word wins; `null` writes nothing. Never
 * throws.
 */
export async function applyEncodingCeiling(
  sender: CeilingSender,
  wanted: () => number | null,
): Promise<{ result: CeilingOutcome; error?: unknown }> {
  try {
    const maxBitrate = wanted();
    if (maxBitrate === null) return { result: "applied" };
    const params = sender.getParameters();
    const first = params.encodings[0];
    if (first === undefined) return { result: "no-encoding" };
    if (first.maxBitrate === maxBitrate) return { result: "applied" };
    params.encodings[0] = { ...first, maxBitrate };
    await sender.setParameters(params);
    return { result: "applied" };
  } catch (error) {
    return { result: "refused", error };
  }
}

/**
 * Keeps one sender's `encodings[0].maxBitrate` at what was last asked.
 *
 *   * **Serialised.** Every write goes through the sender's one queue of
 *     parameter writes (`sender-parameters.ts`), which the `active` flag
 *     (`sender-activity.ts`) and the sender's set-up (`camera-sender.ts`)
 *     use too: a read-change-set never overlaps another on the same sender,
 *     so neither is refused for an outdated reading and neither puts back
 *     what the other just changed.
 *   * **The last word wins.** A write applies what is wanted when it runs.
 *   * **Re-applied.** Every write reads the encoding as the browser holds it
 *     now, so asking again (every counters tick does) puts back a ceiling a
 *     renegotiation, a replaced track or a new sender ({@link bind}) lost.
 *   * **Refused.** A refused write is asked again at every later
 *     {@link want}, and said once per sender.
 *   * **Before the first word** (`want(null)`), nothing is written.
 */
export class SenderCeiling {
  private sender: CeilingSender | null = null;
  private wanted: number | null = null;
  private scheduled = false;
  private chain: Promise<void> = Promise.resolve();
  private warned = false;

  constructor(
    /** Names the sender in the one warning: `main` or `middle`. */
    private readonly name: string,
    private readonly warn: (message: string, err: unknown) => void = (message, err) => {
      // eslint-disable-next-line no-console
      console.warn(message, err ?? "");
    },
  ) {}

  /**
   * The sender this keeps. A different one (first publish, mid-call
   * camera-on, a copy added again) may say its warning. `null`: there is
   * none.
   */
  bind(sender: CeilingSender | null): void {
    if (this.sender === sender) return;
    this.sender = sender;
    this.warned = false;
  }

  /** Whether {@link bind} was given a sender. */
  get bound(): boolean {
    return this.sender !== null;
  }

  /** What was last asked, bits a second; `null` before anything was. */
  get lastWanted(): number | null {
    return this.wanted;
  }

  /**
   * Ask for the encoding's ceiling, bits a second; `null`: nothing is asked
   * (the sender keeps what it holds). Safe to call at any time and as often
   * as wanted: it costs one synchronous `getParameters` when the encoding
   * already says so.
   */
  want(maxBitrate: number | null): void {
    this.wanted = maxBitrate;
    if (this.sender === null || maxBitrate === null || this.scheduled) return;
    this.scheduled = true;
    this.chain = this.chain.then(() => this.step());
  }

  /** Resolves when every write asked for so far has settled. */
  settled(): Promise<void> {
    return this.chain;
  }

  /** Forget the sender and what was asked: the call is over. */
  reset(): void {
    this.sender = null;
    this.wanted = null;
    this.warned = false;
  }

  private async step(): Promise<void> {
    this.scheduled = false;
    const sender = this.sender;
    if (sender === null) return;
    const outcome = await queueParameterWrite(sender, () =>
      // A sender replaced while this waited its turn is not written: the
      // new one is asked by the caller's next `want`.
      applyEncodingCeiling(sender, () => (this.sender === sender ? this.wanted : null)),
    );
    if (this.sender !== sender) return;
    if (outcome.result !== "refused") return;
    if (this.warned) return;
    this.warned = true;
    this.warn(
      `camera sender ceiling: the ${this.name} camera sender's bitrate ceiling could not be set, asking again`,
      outcome.error,
    );
  }
}
