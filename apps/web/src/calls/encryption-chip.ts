// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The in-call encryption chip and the sheet behind it (`docs/ux/calls.md` §4,
 * §5).
 *
 * ## What changed, and why the module was renamed
 *
 * This file was `safety-pill.ts`, and its one rule decided whether to render a
 * chip that **was** the safety number: sixteen digits across the top of the
 * call. §5 retires that outright — "the code is never shown unless the user
 * asks for it" — so the chip now reads `Encrypted` and the digits move into a
 * sheet the chip opens. A module called `safety-pill` exporting
 * `showsSafetyNumber` for a chip that shows no number is the kind of name that
 * quietly stops describing its subject, so it moved with the behaviour.
 *
 * ## Why any of this is a module and not a condition in the JSX
 *
 * Because the predecessor was a condition in the JSX and it was wrong there for
 * nine months without anything noticing. `vitest` runs in a plain `node`
 * environment with no jsdom and no Solid JSX transform, so nothing in
 * `routes/Call.tsx` is reachable from a test. A rule nobody can assert is a
 * rule that drifts.
 *
 * ## The bug that produced that lesson, kept because the reasoning was sound
 *
 * `Call.tsx` gated the pill on `props.embedded !== true`, with a comment giving
 * a correct reason: the **1x1** web path is P2P DTLS-SRTP with no SFrame, so
 * there is no safety number and a "secured" chip would mislead.
 *
 * That reason is right about 1x1 and wrong about everything else, because the
 * same component also renders the **SFU group call**, which does seal media
 * with SFrame and does have a real safety number. And `Chat.tsx` mounts the
 * call `embedded={true}` in every case — so the gate hid the chip on **every
 * live call**, group calls included.
 *
 * ## The rule
 *
 * Show the chip exactly when there **is** a safety number to show. That is not
 * a proxy for the 1x1-vs-group question, it is the question: the 1x1 path
 * publishes `{ active: false, shortFingerprint: null }`
 * (`calls/CallHostContext.tsx`) and the SFU path publishes
 * `{ active: true, shortFingerprint: <derived> }` (`calls/sfu-call-ui.ts`,
 * `calls/multi-joiner-call.ts`). So the misleading-chip hazard the old comment
 * worried about is closed by the data rather than by a layout flag — and a
 * future path that arms SFrame gets the chip automatically instead of needing
 * someone to remember a second gate.
 *
 * **Both halves are load-bearing, and the second one more so now than before.**
 * `active` without a number used to mean an empty chip; under §5 it would mean
 * a chip that opens a sheet with nothing in it, having just promised the user a
 * code to compare. A number without `active` should not occur, and if it does
 * the honest answer is not to claim protection.
 */

/** The call store's E2EE slice, as these rules read it. */
export interface EncryptionChipInput {
  readonly active: boolean;
  readonly shortFingerprint: string | null;
}

/**
 * `true` when the call screen should render the `Encrypted` chip.
 *
 * Deliberately takes no notion of which surface is asking. Whether the call is
 * embedded in a chat or standing alone is a layout question, and the rule this
 * replaces was answering it by accident.
 */
export function showsEncryptionChip(e2ee: EncryptionChipInput): boolean {
  return e2ee.active && safetyNumberGroups(e2ee.shortFingerprint).length > 0;
}

/**
 * The sheet's one plain sentence (§5), naming the other person when this
 * device knows who they are. Word for word what
 * `callEncryptionSentence` says on Android and iOS.
 *
 * A name is what makes the sentence worth reading, so it is used whenever
 * there is one. Where there is not — a group call, or a contact this device
 * has never been able to name — the sentence stays true rather than inventing
 * a person.
 *
 * Web has one extra case the natives do not: its call roster falls back to the
 * literal words `Peer` and `Participant` for an unresolved participant, and
 * "Only you and Peer can hear this call." reads as a claim about somebody
 * called Peer. Those are treated as no name — a platform mechanic, not a
 * different sentence.
 */
export function encryptionSheetSentence(peerName: string | null | undefined): string {
  const name = peerName?.trim() ?? "";
  if (name === "" || name === "Peer" || name === "Participant") {
    return "Only the people on this call can hear it.";
  }
  return `Only you and ${name} can hear this call.`;
}

/** Digits per spoken / displayed group. */
const SAFETY_NUMBER_GROUP_SIZE = 4;

/**
 * The safety number split into its groups of four, for reading aloud (§5:
 * "four groups of four digits, large and grouped for reading aloud").
 *
 * The core (`lakko_calls::safety_number`) already renders four space-separated
 * groups of four digits and pins that shape with a golden vector, so this is
 * normally a split. It re-groups anyway rather than trusting the caller,
 * because the display depends on the grouping being right and a value that
 * arrives unspaced would otherwise render as one unreadable run — the exact
 * failure this whole change exists to remove. Returns an empty list for a
 * blank value, so the sheet can show nothing rather than an empty box.
 */
export function safetyNumberGroups(shortFingerprint: string | null | undefined): string[] {
  const digits = (shortFingerprint ?? "").replace(/\s+/gu, "");
  if (digits.length === 0) return [];
  const out: string[] = [];
  for (let i = 0; i < digits.length; i += SAFETY_NUMBER_GROUP_SIZE) {
    out.push(digits.slice(i, i + SAFETY_NUMBER_GROUP_SIZE));
  }
  return out;
}

/**
 * What a screen reader reads for the safety number (§8: "read in groups of
 * four"). Commas make the reader pause between groups instead of running
 * sixteen digits together.
 */
export function safetyNumberSpokenLabel(shortFingerprint: string | null | undefined): string {
  const groups = safetyNumberGroups(shortFingerprint);
  if (groups.length === 0) return "Safety number unavailable";
  return `Safety number: ${groups.join(", ")}`;
}
