// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Changing a username (ADR-0076): what the screen shows, the too-soon rule,
 * the one resumable operation with its rollback, and the small owed record
 * that lets a crash between steps be finished or rolled back.
 *
 * The operation, in order:
 *
 *   a. the new handle is derived;
 *   b. `PUT /auth/login-handle` moves the account's handle (fresh-auth grant,
 *      the previous handle, and the recovery email re-keyed when this device
 *      holds the address: with a vault freshly sealed under the NEW handle
 *      when no phrase is saved);
 *   c. discovery `username/replace`. When it fails (the name is taken, too
 *      soon, anything), the OLD handle is bound back the same way, the vault
 *      re-sealed under the old handle;
 *   d. the certificate is kept and the name saved on this device (the local
 *      marker, the session, the own-username register);
 *   e. the publishers follow by themselves (their inputs changed).
 *
 * Pure rules and a runner over ports. Nothing here holds the address beyond
 * the call that carries it.
 */
import type { MarkerStorage } from "./phrase-gate";

// ---------------------------------------------------------------------------
// The screen
// ---------------------------------------------------------------------------

/** How the recovery email fares in a rename, on this device. */
export type RenameEmail =
  /** The account has no recovery email. */
  | "none"
  /** It has one and this device holds the address: it is re-keyed with the
   *  name, nothing is lost and nothing is said. */
  | "kept"
  /** It has one and this device does NOT hold the address: the rename turns
   *  it off, and the screen says so (`recovery_email.username_change`). */
  | "dropped";

export function renameEmail(hasRecoveryEmail: boolean, addressHeld: boolean): RenameEmail {
  if (!hasRecoveryEmail) return "none";
  return addressHeld ? "kept" : "dropped";
}

/** The warning lines of the screen, in order (catalog keys). */
export function renameWarningKeys(email: RenameEmail): string[] {
  const keys = ["username.change.body", "username.change.next_change"];
  if (email === "dropped") keys.push("recovery_email.username_change");
  return keys;
}

/** Whether the rename seals a vault: the recovery email is kept and no
 *  phrase is saved (a phrase-protected account has no vault). */
export function renameSealsVault(email: RenameEmail, phraseSaved: boolean): boolean {
  return email === "kept" && !phraseSaved;
}

// ---------------------------------------------------------------------------
// Too soon
// ---------------------------------------------------------------------------

/** A name can be changed once in this long. The server is the authority
 *  (`429 rename-too-soon`); the local stamp only saves a round trip. */
export const RENAME_INTERVAL_MS = 30 * 24 * 60 * 60 * 1000;

/** When the next change is allowed, unix ms, or `null` when it is allowed
 *  now (no known change, or one at least thirty days ago). A stamp in the
 *  future (a clock that went backwards) is not trusted. */
export function renameAllowedAtMs(lastChangedAtMs: number | null, nowMs: number): number | null {
  if (lastChangedAtMs === null || lastChangedAtMs > nowMs) return null;
  const at = lastChangedAtMs + RENAME_INTERVAL_MS;
  return at > nowMs ? at : null;
}

/** The date `429 rename-too-soon` names, from its `retry_after_secs`. */
export function tooSoonUntilMs(retryAfterSecs: number | null, nowMs: number): number {
  return nowMs + Math.max(0, retryAfterSecs ?? RENAME_INTERVAL_MS / 1000) * 1000;
}

function changedAtKey(accountIdB64: string): string {
  return `lakko:username-changed-at:${accountIdB64}`;
}

export function readRenamedAt(storage: MarkerStorage | undefined, accountIdB64: string): number | null {
  try {
    const raw = storage?.getItem(changedAtKey(accountIdB64)) ?? null;
    if (raw === null) return null;
    const ms = Number(raw);
    return Number.isFinite(ms) && ms > 0 ? ms : null;
  } catch {
    return null;
  }
}

export function writeRenamedAt(storage: MarkerStorage | undefined, accountIdB64: string, nowMs: number): void {
  try {
    storage?.setItem(changedAtKey(accountIdB64), String(nowMs));
  } catch {
    // Storage blocked: the server's answer says when.
  }
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

/** Forget both rows (sign-out wipe). */
export function clearRenameStores(storage: MarkerStorage | undefined, accountIdB64: string): void {
  try {
    storage?.removeItem(owedKey(accountIdB64));
    storage?.removeItem(changedAtKey(accountIdB64));
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
  /** `429 rename-too-soon`: allowed again at `untilMs`. */
  | { readonly kind: "too-soon"; readonly untilMs: number }
  /** `403 fresh-auth-required`: the proof block. */
  | { readonly kind: "proof-required" }
  /** `429 handle-bind-limited`. */
  | { readonly kind: "bind-limited"; readonly retryAfterSecs: number | null }
  /** `403 address-mismatch`: the address this device holds is not the
   *  account's. It is cleared; the screen then warns about the email. */
  | { readonly kind: "address-mismatch" }
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
  if (answer.status === 429) return { kind: "bind-limited", retryAfterSecs: answer.retryAfterSecs };
  return { kind: "error" };
}

/** Read a refused `username/replace` (step c). */
export function replaceFailure(answer: Answer, nowMs: number): RenameFailure {
  if (answer.status === null) return { kind: "offline" };
  if (answer.status === 409 && answer.errorCode === "username-taken") return { kind: "taken" };
  if (answer.status === 429 && answer.errorCode === "rename-too-soon") {
    return { kind: "too-soon", untilMs: tooSoonUntilMs(answer.retryAfterSecs, nowMs) };
  }
  return { kind: "error" };
}

/** The recovery email as a bind carries it. */
export interface RenameEmailPart {
  readonly address: string;
  /** Present only when a vault is sealed (no phrase saved). */
  readonly vault?: { readonly salt: Uint8Array; readonly sealed: Uint8Array };
}

export interface RenamePorts {
  /** The handle of a name, in its wire form and its 32 bytes. */
  readonly handleFor: (username: string) => Promise<{ readonly bytes: Uint8Array; readonly wire: string }>;
  /** Seal the current phrase under `handle` and the address, with a new
   *  random salt, off the main thread. `null` when it could not be sealed. */
  readonly sealVault: (
    handle: Uint8Array,
    address: string,
  ) => Promise<{ readonly salt: Uint8Array; readonly sealed: Uint8Array } | null>;
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
  /** Step d: the name is this device's (marker, session, register). */
  readonly finishLocal: (username: string, certificate: Uint8Array | null) => Promise<void>;
  readonly nowMs: () => number;
}

export type RenameOutcome =
  | { readonly kind: "done"; readonly username: string }
  | { readonly kind: "failed"; readonly failure: RenameFailure; readonly rolledBack: boolean | null };

/** The recovery email part of a bind to `handle`, sealing when asked. */
async function emailPart(
  ports: RenamePorts,
  handle: Uint8Array,
  email: { readonly address: string; readonly sealVault: boolean } | null,
): Promise<RenameEmailPart | null | "unsealed"> {
  if (email === null) return null;
  if (!email.sealVault) return { address: email.address };
  const vault = await ports.sealVault(handle, email.address);
  return vault === null ? "unsealed" : { address: email.address, vault };
}

/**
 * Bind the OLD handle back after a refused replace: the same shape, with the
 * previous handle the new one and the vault re-sealed under the old handle.
 * Resolves whether it went through.
 */
export async function rollbackRename(
  ports: RenamePorts,
  input: {
    readonly oldUsername: string;
    readonly newUsername: string;
    readonly grant: string;
    readonly email: { readonly address: string; readonly sealVault: boolean } | null;
  },
): Promise<boolean> {
  try {
    const oldHandle = await ports.handleFor(input.oldUsername);
    const newHandle = await ports.handleFor(input.newUsername);
    const part = await emailPart(ports, oldHandle.bytes, input.email);
    if (part === "unsealed") return false;
    const refused = await ports.bind({
      handle: oldHandle.wire,
      previousHandle: newHandle.wire,
      grant: input.grant,
      email: part,
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
    /** The recovery email to carry over, or `null` (none, or this device
     *  does not hold the address: the server then drops it). */
    readonly email: { readonly address: string; readonly sealVault: boolean } | null;
  },
): Promise<RenameOutcome> {
  const failed = (failure: RenameFailure, rolledBack: boolean | null = null): RenameOutcome => ({
    kind: "failed",
    failure,
    rolledBack,
  });
  let oldHandle: { readonly bytes: Uint8Array; readonly wire: string };
  let newHandle: { readonly bytes: Uint8Array; readonly wire: string };
  let part: RenameEmailPart | null | "unsealed";
  try {
    oldHandle = await ports.handleFor(input.oldUsername);
    newHandle = await ports.handleFor(input.newUsername);
    part = await emailPart(ports, newHandle.bytes, input.email);
  } catch {
    return failed({ kind: "error" });
  }
  // Nothing is sent when the vault could not be sealed: a bind without it
  // would leave a mail-protected account without its keys' copy.
  if (part === "unsealed") return failed({ kind: "error" });

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
    const failure = replaceFailure(replaced.answer, ports.nowMs());
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
