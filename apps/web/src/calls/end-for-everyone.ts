// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Ending a group call for everyone (`docs/ux/calls.md` section 4.1.2).
 *
 * The person who started a group or link call gets a choice behind `End`:
 * leave, or end the call for every person in it. The end travels as an
 * ordinary `CallEnd` with the reason `endedForAll`, inside the conversation's
 * encryption (the call's own group for a link call), and a device takes itself
 * out of the call only when the MLS-authenticated sender is that call's
 * starter. The same message from anyone else is an ordinary end notice and
 * takes nobody out. The server is never asked.
 *
 * Pure apart from the catalog lookup: no wasm, no storage, no Solid.
 */
import { t } from "../l10n";

/** The wire value of the reason (`composeCallEndSignaling`). */
export const ENDED_FOR_ALL = "endedForAll";

/**
 * Whether pressing `End` opens the choice rather than ending the call.
 *
 * Only for the starter, and only while someone else is in the call: alone,
 * both rows would do the same thing.
 */
export function endOpensMenu(isStarter: boolean, othersInCall: number): boolean {
  return isStarter && othersInCall > 0;
}

/** What a received end says about the call this device is in. */
export interface ReceivedEnd {
  /** `ParsedCallSignaling.endReason`. */
  readonly endReason: string;
  /** The sender's person key, resolved from the MLS sender at receipt;
   *  `undefined` when it could not be resolved. */
  readonly senderPersonKey: string | undefined;
  /** The person who started the call, as this device recorded it; `null`
   *  when it does not know. */
  readonly starterPersonKey: string | null;
}

/**
 * Whether a received end takes this device out of the call: the reason says
 * "for everyone" AND it comes from the call's starter. Anything less is not
 * enough: an unknown starter, an unresolved sender, or anyone else.
 */
export function endsTheCallForMe(end: ReceivedEnd): boolean {
  return (
    end.endReason === ENDED_FOR_ALL &&
    end.senderPersonKey !== undefined &&
    end.senderPersonKey.length > 0 &&
    end.starterPersonKey !== null &&
    end.senderPersonKey === end.starterPersonKey
  );
}

/** The line under `Call ended` for the others. */
export function endedForEveryoneLine(starterName: string | null): string {
  const name = starterName?.trim() ?? "";
  return name.length > 0
    ? t("call.ended.for_all_by", { name })
    : t("call.ended.for_all");
}
