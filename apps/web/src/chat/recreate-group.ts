// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0065 phase 2 (UX 16.5) — the Recreate group sheet's pure decisions:
 * which rows the member list holds, what each is pre-set to, and which may
 * be changed.
 *
 * The sheet is the new-group flow pre-filled. Pre-filled with every current
 * member except the old owner (the person whose keys were reset, under the
 * OLD key the roster holds); the creating admin becomes the new group's
 * owner by the ordinary rule of creation and is never a row. The reset
 * person's NEW identity is listed unticked and disabled with the two-line
 * reason while `reset-unverified` on this device, and ticked and enabled
 * once this device has verified them. A member whose account id this device
 * does not hold (a pre-ADR-0021 contact, a stranger admitted by someone
 * else) is listed, dimmed and unselectable, with the new-group flow's own
 * line, because the add path needs the account id.
 *
 * Pure: no wasm, no Solid, no storage.
 */

/** What the caller knows about one person in the roster. */
export interface RecreateMember {
  /** The person key the roster holds, base64. */
  readonly personKeyB64: string;
  readonly label: string;
  /** The account id (32 lower-case hex), when a contact row carries it. */
  readonly accountIdHex: string | undefined;
  /** The signature key the add path takes: the contact ROW's key, which is
   *  the person's CURRENT key (after a verified reset the row was re-keyed). */
  readonly signaturePubkeyB64: string | undefined;
}

/** One row of the sheet's member list. */
export interface RecreateRow {
  readonly personKeyB64: string;
  readonly label: string;
  readonly accountIdHex: string | undefined;
  readonly signaturePubkeyB64: string | undefined;
  /** Pre-set tick. */
  readonly checked: boolean;
  /** Whether the tick may be changed. */
  readonly enabled: boolean;
  /** Why the row is not selectable, or `null`. */
  readonly reason: "keys-reset-unverified" | "no-account-id" | null;
}

/**
 * Build the rows (UX 16.5).
 *
 * `oldOwnerKeyB64` is the reset person's OLD key (`previousKey`), excluded
 * from the roster rows; `resetContact` is their NEW identity as this device
 * knows it — present when the person is a contact with an account id — with
 * `unverified` from the contact store. `ownPersonKeyB64` is never a row.
 */
export function recreateRows(input: {
  readonly roster: readonly RecreateMember[];
  readonly ownPersonKeyB64: string | null;
  readonly oldOwnerKeyB64: string;
  readonly resetContact:
    | {
        readonly label: string;
        readonly accountIdHex: string;
        readonly signaturePubkeyB64: string;
        readonly unverified: boolean;
      }
    | null;
}): RecreateRow[] {
  const rows: RecreateRow[] = [];
  for (const m of input.roster) {
    if (m.personKeyB64 === input.ownPersonKeyB64) continue;
    if (m.personKeyB64 === input.oldOwnerKeyB64) continue;
    // The reset person's NEW keys may already be in the roster (some admin
    // added them); they get the reset row below, not an ordinary one.
    if (input.resetContact !== null && m.personKeyB64 === input.resetContact.signaturePubkeyB64) {
      continue;
    }
    const addable = m.accountIdHex !== undefined && m.signaturePubkeyB64 !== undefined;
    rows.push({
      personKeyB64: m.personKeyB64,
      label: m.label,
      accountIdHex: m.accountIdHex,
      signaturePubkeyB64: m.signaturePubkeyB64,
      checked: addable,
      enabled: addable,
      reason: addable ? null : "no-account-id",
    });
  }
  if (input.resetContact !== null) {
    const c = input.resetContact;
    rows.push({
      personKeyB64: c.signaturePubkeyB64,
      label: c.label,
      accountIdHex: c.accountIdHex,
      signaturePubkeyB64: c.signaturePubkeyB64,
      checked: !c.unverified,
      enabled: !c.unverified,
      reason: c.unverified ? "keys-reset-unverified" : null,
    });
  }
  return rows;
}

/** The members the submit invites: the ticked, addable rows, in list
 *  order. The first founds the group; the rest are added afterwards. */
export function recreateInvitees(
  rows: readonly RecreateRow[],
  ticked: ReadonlySet<string>,
): Array<{ accountIdHex: string; signaturePubkeyB64: string; label: string }> {
  const out: Array<{ accountIdHex: string; signaturePubkeyB64: string; label: string }> = [];
  for (const r of rows) {
    if (!r.enabled || !ticked.has(r.personKeyB64)) continue;
    if (r.accountIdHex === undefined || r.signaturePubkeyB64 === undefined) continue;
    out.push({ accountIdHex: r.accountIdHex, signaturePubkeyB64: r.signaturePubkeyB64, label: r.label });
  }
  return out;
}

/** The submit is enabled once at least one member is ticked (UX 16.5 Idle). */
export function recreateSubmitEnabled(
  rows: readonly RecreateRow[],
  ticked: ReadonlySet<string>,
): boolean {
  return recreateInvitees(rows, ticked).length > 0;
}

/** The initial tick set: every row pre-set to checked. */
export function initialTicks(rows: readonly RecreateRow[]): Set<string> {
  return new Set(rows.filter((r) => r.checked).map((r) => r.personKeyB64));
}
