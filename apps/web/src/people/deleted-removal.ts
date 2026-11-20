// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0098 section 5 — whether this device proposes the removal of a deleted
 * account's leaves from one group, and which leaf it names (the remove flow
 * removes every leaf of that account, ADR-0050).
 *
 * Only a verified statement leads here, never a directory answer: a directory
 * must not be able to make admins remove a live member. Pure.
 */

/** What the plan reads. */
export interface RemovalFacts {
  /** `chat` (a one-to-one) is never touched (ADR-0062). */
  readonly kind: "chat" | "group";
  /** This device's role in the group. */
  readonly ownRole: "owner" | "admin" | "member" | null;
  /** base64 account key of the group's owner, when the group has one. */
  readonly ownerKeyB64: string | undefined;
  /** base64 account key of the deleted person. */
  readonly deletedKeyB64: string;
  /** The roster: each leaf and the person key it resolves to. */
  readonly roster: readonly { readonly leafIndex: number; readonly personKeyB64: string }[];
  /** Leaves a remove proposal already names (pending in the group). */
  readonly pendingRemoveLeaves: readonly number[];
  /** This device already sent this removal for this group. */
  readonly alreadyProposed: boolean;
}

/** What to do: propose naming `leafIndex`, nothing (`none`), or the person
 *  is no longer in the group (`gone`: the removal is done). */
export type RemovalPlan =
  | { readonly kind: "propose"; readonly leafIndex: number }
  | { readonly kind: "none" }
  | { readonly kind: "gone" };

export function deletedRemovalPlan(f: RemovalFacts): RemovalPlan {
  const leaves = f.roster.filter((m) => m.personKeyB64 === f.deletedKeyB64);
  if (leaves.length === 0) return { kind: "gone" };
  if (f.kind !== "group") return { kind: "none" };
  if (f.ownRole !== "owner" && f.ownRole !== "admin") return { kind: "none" };
  // C-63: the owner's leaves are unremovable.
  if (f.ownerKeyB64 !== undefined && f.ownerKeyB64 === f.deletedKeyB64) return { kind: "none" };
  if (f.alreadyProposed) return { kind: "none" };
  if (leaves.every((m) => f.pendingRemoveLeaves.includes(m.leafIndex))) return { kind: "none" };
  const first = leaves[0];
  return first === undefined ? { kind: "gone" } : { kind: "propose", leafIndex: first.leafIndex };
}
