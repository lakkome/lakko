// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `docs/ux/message-actions.md` section 4.1 — the `Pin` / `Unpin` row of the
 * action card and the two-row sheet behind it, decided once here so the row
 * label, the sheet's rows and the act-at-once cases cannot disagree.
 *
 * The inputs are what this device knows about ONE message: whether it is
 * pinned for everyone, whether it is pinned for me, whether this device may
 * set a shared pin at all (owner or admin of a group's role table, any
 * member of a table-less group, either participant of a one-to-one), and
 * whether the conversation is a one-to-one (the shared row then reads
 * "for both").
 *
 * The rule, as the spec states it:
 *
 *   * neither scope: the row reads `Pin`; the sheet offers `Pin for me` and,
 *     when allowed, `Pin for everyone` / `Pin for both`. A plain member sees
 *     the one-row sheet "and no explanation; the missing row is the
 *     explanation";
 *   * one scope: the row reads `Unpin` and acts at once — except a shared
 *     pin this device may NOT clear, where the row reads `Pin` and the sheet
 *     offers `Pin for me` alone;
 *   * both scopes: `Unpin`, and the sheet asks which — `Unpin for me` and,
 *     when allowed, `Unpin for everyone` / `Unpin for both`. A plain member
 *     holding a message pinned by an admin sees `Unpin for me` only, at once.
 */

/** The state of one message this device knows. */
export interface PinState {
  readonly pinnedShared: boolean;
  readonly pinnedPersonal: boolean;
  /** Whether this device may set or clear a shared pin here. */
  readonly maySetShared: boolean;
  readonly oneToOne: boolean;
}

/** One row of the sheet. */
export interface PinOption {
  readonly scope: "personal" | "shared";
  /** `true` = pin, `false` = unpin. */
  readonly pinned: boolean;
  readonly label: string;
  /** The one line under the row saying what it does. */
  readonly detail: string;
}

/** What tapping the card row does. */
export type PinAction =
  | { readonly kind: "immediate"; readonly option: PinOption }
  | { readonly kind: "sheet"; readonly title: string; readonly options: readonly PinOption[] };

/** The row and the action behind it. */
export interface PinRowDecision {
  readonly row: "Pin" | "Unpin";
  readonly action: PinAction;
}

export const PIN_DETAIL_PERSONAL = "Only on this device";
export const PIN_DETAIL_SHARED = "Every member sees it";
export const PIN_SHEET_TITLE = "Pin message";
export const UNPIN_SHEET_TITLE = "Unpin message";

function option(scope: PinOption["scope"], pinned: boolean, oneToOne: boolean): PinOption {
  const verb = pinned ? "Pin" : "Unpin";
  if (scope === "personal") {
    return { scope, pinned, label: `${verb} for me`, detail: PIN_DETAIL_PERSONAL };
  }
  return {
    scope,
    pinned,
    label: oneToOne ? `${verb} for both` : `${verb} for everyone`,
    detail: PIN_DETAIL_SHARED,
  };
}

/** Section 4.1 "The row" + "The sheet", as one decision. */
export function pinRowDecision(state: PinState): PinRowDecision {
  const { pinnedShared, pinnedPersonal, maySetShared, oneToOne } = state;
  // A shared pin this device may not clear is, for the purpose of this row,
  // not something it can unpin at all.
  const clearableShared = pinnedShared && maySetShared;
  if (clearableShared && pinnedPersonal) {
    return {
      row: "Unpin",
      action: {
        kind: "sheet",
        title: UNPIN_SHEET_TITLE,
        options: [option("personal", false, oneToOne), option("shared", false, oneToOne)],
      },
    };
  }
  if (clearableShared) {
    return { row: "Unpin", action: { kind: "immediate", option: option("shared", false, oneToOne) } };
  }
  if (pinnedPersonal) {
    return { row: "Unpin", action: { kind: "immediate", option: option("personal", false, oneToOne) } };
  }
  // Nothing this device may unpin: offer to pin. The shared row is offered
  // only when allowed AND not already set (a plain member holding a message
  // an admin pinned gets `Pin for me` alone).
  const options: PinOption[] = [option("personal", true, oneToOne)];
  if (maySetShared && !pinnedShared) options.push(option("shared", true, oneToOne));
  return { row: "Pin", action: { kind: "sheet", title: PIN_SHEET_TITLE, options } };
}
