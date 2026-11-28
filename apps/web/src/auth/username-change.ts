// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Changing a username (ADR-0076; no minimum interval since 2026-10-26): how
 * the recovery email fares, the one resumable operation with its rollback,
 * and the small owed record
 * that lets a crash between steps be finished or rolled back.
 *
 * The operation, in order:
 *
 *   a. the new handle is derived;
 *   b. `PUT /auth/login-handle` moves the account's handle (fresh-auth grant,
 *      the previous handle, and the recovery email re-keyed when this device
 *      holds the address or the person just typed it). No vault rides along
 *      since 2026-11-26 (ADR-0075, second amendment);
 *   c. discovery `username/replace`. When it fails (the name is taken, or
 *      anything else), the OLD handle is bound back the same way;
 *   d. the certificate is kept and the name saved on this device (the
 *      session, the own-username register);
 *   e. the publishers follow by themselves (their inputs changed).
 *
 * Pure rules and a runner over ports. Nothing here holds the address beyond
 * the call that carries it.
 */
import type { MarkerStorage } from "./phrase-gate";

// ---------------------------------------------------------------------------
// The recovery email
// ---------------------------------------------------------------------------

/** How the recovery email fares in a rename, on this device. */
export type RenameEmail =
  /** The account has no recovery email. */
  | "none"
  /** It has one and this device holds the address: it is re-keyed with the
   *  name, nothing is lost and nothing is said. */
  | "kept"
  /** It has one and this device does NOT hold the address: the rename turns
   *  it off. */
  | "dropped";

export function renameEmail(hasRecoveryEmail: boolean, addressHeld: boolean): RenameEmail {
  if (!hasRecoveryEmail) return "none";
  return addressHeld ? "kept" : "dropped";
}

/**
 * Whether the username screen asks for the recovery email before the change
 * (UX 19.1c, `protect.username_email`): the email is the account's LAST
 * second way in and this device holds no address to carry into the change.
 * The server would refuse the change otherwise (`409 last-factor`), since a
 * rename without the address drops the email. `lastFactor` is
 * `removingLeavesNone(factors, "email")` (`protect/factors.ts`); unknown
 * factors ask for nothing here and the server decides.
 */
export function renameNeedsAddress(email: RenameEmail, lastFactor: boolean): boolean {
  return email === "dropped" && lastFactor;
}

// ---------------------------------------------------------------------------
// The owed record
// ---------------------------------------------------------------------------

/** Where an unfinished rename stands. Names only: no address, no grant. */
export interface RenameOwed {
  readonly v: 1;
  readonly oldUsername: string;
  readonly newUsername: string;
  /**
   * - `binding`: the handle bind was (about to be) sent; whether it and the
   *   replace landed is not known.
   * - `replaced`: both landed; only this device's own state is to be moved.
   * - `rollback`: the replace was refused and binding the old handle back
   *   did not go through: the account's handle is the NEW name's while its
   *   name is the OLD one. The person finishes the rollback (it needs the
   *   proof).
   */
  readonly phase: "binding" | "replaced" | "rollback";
}

function owedKey(accountIdB64: string): string {
  return `lakko:username-change-owed:${accountIdB64}`;
}

export function readRenameOwed(storage: MarkerStorage | undefined, accountIdB64: string): RenameOwed | null {
  try {
    const raw = storage?.getItem(owedKey(accountIdB64)) ?? null;
    if (raw === null) return null;
    const r = JSON.parse(raw) as Partial<RenameOwed>;
    if (
      r.v !== 1 ||
      typeof r.oldUsername !== "string" ||
      typeof r.newUsername !== "string" ||
      r.oldUsername.length === 0 ||
      r.newUsername.length === 0 ||
      (r.phase !== "binding" && r.phase !== "replaced" && r.phase !== "rollback")
    ) {
      return null;
    }
    return { v: 1, oldUsername: r.oldUsername, newUsername: r.newUsername, phase: r.phase };
  } catch {
    return null;
  }
}

export function writeRenameOwed(
  storage: MarkerStorage | undefined,
  accountIdB64: string,
  record: RenameOwed | null,
): void {
  try {
    if (record === null) storage?.removeItem(owedKey(accountIdB64));
    else storage?.setItem(owedKey(accountIdB64), JSON.stringify(record));
  } catch {
    // Storage blocked: the operation still runs to its end in this page.
  }
}

/** Forget the owed record (sign-out wipe). */
export function clearRenameStores(storage: MarkerStorage | undefined, accountIdB64: string): void {
  try {
    storage?.removeItem(owedKey(accountIdB64));
  } catch {
    // Storage blocked: nothing was recorded either.
  }
}

/**
 * What a landing does with an owed record, given what auth says about the
 * NEW name's handle.
 *
 *   - `replaced`: finish on this device (the servers are done).
 *   - `binding`, the account does not hold the new handle: the bind never
 *     landed, nothing changed. Drop the record.
 *   - `binding`, the account holds the new handle: the bind landed and the
 *     replace is not known. Send the replace again (it needs no proof); it
 *     either finishes the rename or shows that the old handle must be bound
 *     back, which is the person's to do.
 *   - `rollback`: waits for the person (the proof).
 *   - no answer: asked again at the next landing.
 */
export function renameResume(
  record: RenameOwed,
  newHandleState: "same" | "other" | "none" | null,
): "finish-local" | "drop" | "replace-again" | "wait-for-person" | "later" {
  if (record.phase === "replaced") return "finish-local";
  if (record.phase === "rollback") return "wait-for-person";
  if (newHandleState === null) return "later";
  return newHandleState === "same" ? "replace-again" : "drop";
}

// ---------------------------------------------------------------------------
// The operation
// ---------------------------------------------------------------------------

/** Why a rename did not go through. */
export type RenameFailure =
  | { readonly kind: "taken" }
  /** `403 fresh-auth-required`: the proof block. */
  | { readonly kind: "proof-required" }
  /** `429 handle-bind-limited`. */
  | { readonly kind: "bind-limited"; readonly retryAfterSecs: number | null }
  /** `403 address-mismatch`: the address this device holds is not the
   *  account's. It is cleared, so the next rename carries no address. */
  | { readonly kind: "address-mismatch" }
  /** `409 last-factor`: the change would drop the recovery email, which is
   *  the account's last second way in. Nothing changed; the username screen
   *  asks for the address (`protect.username_email`). */
  | { readonly kind: "last-factor" }
  | { readonly kind: "offline" }
  | { readonly kind: "error" };

export interface Answer {
  readonly status: number | null;
  readonly errorCode: string | null;
  readonly retryAfterSecs: number | null;
}

/** Read a refused handle bind (step b). */
export function bindFailure(answer: Answer): RenameFailure {
  if (answer.status === null) return { kind: "offline" };
  if (answer.status === 403 && answer.errorCode === "fresh-auth-required") return { kind: "proof-required" };
  if (answer.status === 403 && answer.errorCode === "address-mismatch") return { kind: "address-mismatch" };
  if (answer.status === 409 && answer.errorCode === "handle-taken") return { kind: "taken" };
  if (answer.status === 409 && answer.errorCode === "last-factor") return { kind: "last-factor" };
  if (answer.status === 429) return { kind: "bind-limited", retryAfterSecs: answer.retryAfterSecs };
  return { kind: "error" };
}

/** Read a refused `username/replace` (step c). */
export function replaceFailure(answer: Answer): RenameFailure {
  if (answer.status === null) return { kind: "offline" };
  if (answer.status === 409 && answer.errorCode === "username-taken") return { kind: "taken" };
  return { kind: "error" };
}

/** The recovery email as a bind carries it: the address, so the stored
 *  value moves to the new handle with no letter (ADR-0076 section 2). */
export interface RenameEmailPart {
  readonly address: string;
}

export interface RenamePorts {
  /** The handle of a name, in its wire form and its 32 bytes. */
  readonly handleFor: (username: string) => Promise<{ readonly bytes: Uint8Array; readonly wire: string }>;
  /** `PUT /auth/login-handle` as a change. Resolves `null` on success,
   *  else the answer. Never throws. */
  readonly bind: (args: {
    readonly handle: string;
    readonly previousHandle: string;
    readonly grant: string;
    readonly email: RenameEmailPart | null;
  }) => Promise<Answer | null>;
  /** Discovery `username/replace`. Never throws. */
  readonly replace: (
    username: string,
  ) => Promise<
    | { readonly ok: true; readonly username: string; readonly certificate: Uint8Array | null }
    | { readonly ok: false; readonly answer: Answer }
  >;
  /** Persist the owed record (or drop it: `null`). */
  readonly saveOwed: (record: RenameOwed | null) => void;
  /** Step d: the name is this device's (session, register). */
  readonly finishLocal: (username: string, certificate: Uint8Array | null) => Promise<void>;
}

export type RenameOutcome =
  | { readonly kind: "done"; readonly username: string }
  | { readonly kind: "failed"; readonly failure: RenameFailure; readonly rolledBack: boolean | null };

/** The recovery email part of a bind. */
function emailPart(email: { readonly address: string } | null): RenameEmailPart | null {
  return email === null ? null : { address: email.address };
}

/**
 * Bind the OLD handle back after a refused replace: the same shape, with the
 * previous handle the new one. Resolves whether it went through.
 */
export async function rollbackRename(
  ports: RenamePorts,
  input: {
    readonly oldUsername: string;
    readonly newUsername: string;
    readonly grant: string;
    readonly email: { readonly address: string } | null;
  },
): Promise<boolean> {
  try {
    const oldHandle = await ports.handleFor(input.oldUsername);
    const newHandle = await ports.handleFor(input.newUsername);
    const refused = await ports.bind({
      handle: oldHandle.wire,
      previousHandle: newHandle.wire,
      grant: input.grant,
      email: emailPart(input.email),
    });
    return refused === null;
  } catch {
    return false;
  }
}

export async function runRename(
  ports: RenamePorts,
  input: {
    readonly oldUsername: string;
    readonly newUsername: string;
    readonly grant: string;
    /** The recovery email to carry over (the address this device holds, or
     *  the one the person just typed), or `null` (none: the server then
     *  drops it, and refuses the change when it is the last second way in). */
    readonly email: { readonly address: string } | null;
  },
): Promise<RenameOutcome> {
  const failed = (failure: RenameFailure, rolledBack: boolean | null = null): RenameOutcome => ({
    kind: "failed",
    failure,
    rolledBack,
  });
  let oldHandle: { readonly bytes: Uint8Array; readonly wire: string };
  let newHandle: { readonly bytes: Uint8Array; readonly wire: string };
  try {
    oldHandle = await ports.handleFor(input.oldUsername);
    newHandle = await ports.handleFor(input.newUsername);
  } catch {
    return failed({ kind: "error" });
  }
  const part = emailPart(input.email);

  // b. Written before anything is sent.
  const owed: RenameOwed = { v: 1, oldUsername: input.oldUsername, newUsername: input.newUsername, phase: "binding" };
  ports.saveOwed(owed);
  const refused = await ports.bind({
    handle: newHandle.wire,
    previousHandle: oldHandle.wire,
    grant: input.grant,
    email: part,
  });
  if (refused !== null) {
    const failure = bindFailure(refused);
    // The server chose an answer: nothing changed. No answer: whether it
    // landed is asked at the next landing, so the record stays.
    if (failure.kind !== "offline") ports.saveOwed(null);
    return failed(failure);
  }

  // c.
  const replaced = await ports.replace(input.newUsername);
  if (!replaced.ok) {
    const failure = replaceFailure(replaced.answer);
    if (failure.kind === "offline") {
      // The replace may have landed: the record stays and the next landing
      // sends it again.
      return failed(failure);
    }
    const rolledBack = await rollbackRename(ports, input);
    ports.saveOwed(rolledBack ? null : { ...owed, phase: "rollback" });
    return failed(failure, rolledBack);
  }

  // d.
  ports.saveOwed({ ...owed, newUsername: replaced.username, phase: "replaced" });
  await ports.finishLocal(replaced.username, replaced.certificate);
  ports.saveOwed(null);
  return { kind: "done", username: replaced.username };
}
