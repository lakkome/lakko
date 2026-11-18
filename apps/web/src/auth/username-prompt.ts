// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Learning the account's username after a session landed with none
 * (`docs/ux/identity-recovery.md` section 8b).
 *
 * A passkey sign-in on a new device needs no name typed, so the session's
 * username is empty. Until the device knows it, it has no login handle, so
 * it cannot knock on the account's DeviceGroup (ADR-0081: it waits, retrying)
 * and shows no name. The rule, the same on every client:
 *
 * 1. **From the backup first.** The username the restored own profile names,
 *    then the own-username register of the stored synced state. Each is
 *    checked with auth (step 3); `same` is adopted with no screen.
 * 2. **Else ask** with the username prompt, once the session has landed; the
 *    app works behind it.
 * 3. **Check** a name: the username rule (the core, no request for a name it
 *    refuses), its login handle, then `POST /auth/login-handle/check` with
 *    the session, which compares it with THIS account's handle only:
 *    `same` -> adopt; `other` -> "not the username of this account";
 *    `none` -> the account has no username, close; `429` -> too many tries;
 *    anything else -> a network error.
 * 4. **"I don't remember"** closes the prompt after a note; the next start of
 *    the app asks again while the name is still unknown.
 *
 * Neither the name nor its handle is ever logged.
 *
 * Rules live here, markup in `UsernamePrompt.tsx`: `vitest` runs with no
 * jsdom and no Solid transform.
 */
import { createSignal, type Accessor } from "solid-js";

import { AuthHttpError } from "./client";
import { LoginHandleUsernameError, type NameTarget } from "./login-handle";
import type { HandleState } from "./login-handle-sync";

/** What checking one name answered. */
export type UsernameCheck =
  /** The account holds this name's handle. `username` is canonical. */
  | { readonly kind: "same"; readonly username: string }
  /** The account holds another handle. */
  | { readonly kind: "other" }
  /** The account holds no handle at all. */
  | { readonly kind: "none" }
  /** `429`: the per-account check limit. */
  | { readonly kind: "too-many" }
  /** No answer: network, or any other server error. */
  | { readonly kind: "network" }
  /** The username rule refuses the name; no request was made. `message` is
   *  the rule's existing copy. */
  | { readonly kind: "invalid"; readonly message: string };

/** The collaborators a check needs (the app passes the core and auth). */
export interface UsernameCheckDeps {
  /** The canonical name and its handle; throws `LoginHandleUsernameError`
   *  for a name the rule refuses. */
  readonly nameTarget: (typed: string) => Promise<NameTarget>;
  /** `POST /auth/login-handle/check` with `sessionToken`. */
  readonly check: (sessionToken: string, handleWire: string) => Promise<HandleState>;
}

/** Check `typed` against this account. Never throws. */
export async function checkUsername(
  deps: UsernameCheckDeps,
  sessionToken: string,
  typed: string,
): Promise<UsernameCheck> {
  let target: NameTarget;
  try {
    target = await deps.nameTarget(typed);
  } catch (err) {
    if (err instanceof LoginHandleUsernameError) return { kind: "invalid", message: err.message };
    return { kind: "network" };
  }
  try {
    const state = await deps.check(sessionToken, target.handle.wire);
    if (state === "same") return { kind: "same", username: target.username };
    return { kind: state };
  } catch (err) {
    if (err instanceof AuthHttpError && err.status === 429) return { kind: "too-many" };
    return { kind: "network" };
  }
}

/** The names the backup gave, in the order they are tried, each once.
 *  Empty ones are dropped. */
export function backupUsernameCandidates(names: readonly (string | null | undefined)[]): string[] {
  const out: string[] = [];
  for (const name of names) {
    const trimmed = (name ?? "").trim();
    if (trimmed.length > 0 && !out.includes(trimmed)) out.push(trimmed);
  }
  return out;
}

/** What the landing does once the backup's names were checked. */
export type BackupUsernameResolution =
  /** A name the account holds: adopt it, no screen. */
  | { readonly kind: "adopt"; readonly username: string }
  /** Auth says the account has no username at all: nothing to ask. */
  | { readonly kind: "no-username" }
  /** Nothing usable: ask the person. */
  | { readonly kind: "ask" };

/** Step 1: try the backup's names in order. A name auth does not confirm
 *  (another, refused, unanswered) is passed over; none left -> ask. */
export async function resolveFromBackup(
  deps: UsernameCheckDeps,
  sessionToken: string,
  candidates: readonly string[],
): Promise<BackupUsernameResolution> {
  for (const name of candidates) {
    const result = await checkUsername(deps, sessionToken, name);
    if (result.kind === "same") return { kind: "adopt", username: result.username };
    if (result.kind === "none") return { kind: "no-username" };
  }
  return { kind: "ask" };
}

/** The catalog key of a check's error line, `null` for a check that closes
 *  the prompt (or an invalid name, whose copy is the rule's own). */
export function usernamePromptErrorKey(kind: UsernameCheck["kind"]): string | null {
  switch (kind) {
    case "other":
      return "auth.username_prompt.error.not_this";
    case "too-many":
      return "auth.username_prompt.error.too_many";
    case "network":
      return "auth.username_prompt.error.network";
    default:
      return null;
  }
}

/** Whether the landing should look for the username now. */
export function shouldLookForUsername(input: {
  /** A session with its keys has landed. */
  readonly sessionLanded: boolean;
  /** The session's username. */
  readonly username: string;
  /** A guest account (ADR-0032) has no username by design. */
  readonly guest: boolean;
  /** The landing's backup step has finished, so what it gave is known. */
  readonly backupSettled: boolean;
}): boolean {
  return input.sessionLanded && input.username.trim().length === 0 && !input.guest && input.backupSettled;
}

/** What the prompt shows. */
export type UsernamePromptView =
  | { readonly phase: "hidden" }
  /** The field. `error` is a catalog key, or the rule's own copy when
   *  `errorIsText`. */
  | {
      readonly phase: "asking";
      readonly busy: boolean;
      readonly error: string | null;
      readonly errorIsText: boolean;
    }
  /** "I don't remember": the note, then closed. */
  | { readonly phase: "forgot" };

export interface UsernamePromptPorts extends UsernameCheckDeps {
  /** The live session token, or `null` when no session stands. */
  readonly sessionToken: () => string | null;
  /** The names the backup gave (restored profile, stored register). */
  readonly backupCandidates: (accountIdB64: string) => Promise<readonly string[]>;
  /** Keep a name auth confirmed (`AuthApi.adoptVerifiedUsername`). */
  readonly adopt: (username: string) => Promise<void>;
  /** Log sink; never given a name or a handle. */
  readonly warn?: (message: string) => void;
}

export interface UsernamePromptController {
  readonly view: Accessor<UsernamePromptView>;
  /** Look for the username of `accountIdB64` (backup first, then ask). Once
   *  per account in this start of the app; a second call does nothing. */
  start(accountIdB64: string): Promise<void>;
  /** The person pressed Continue with `typed`. */
  submit(typed: string): Promise<void>;
  /** The person pressed "I don't remember". */
  forgot(): void;
  /** Close the prompt (after the forgot note, or the session's name became
   *  known some other way). Asked again only at the next start. */
  close(): void;
  /** The account's memory went (sign-out, Forget device): the next landing
   *  of it looks again. */
  reset(): void;
}

export function createUsernamePromptController(ports: UsernamePromptPorts): UsernamePromptController {
  const [view, setView] = createSignal<UsernamePromptView>({ phase: "hidden" });
  const looked = new Set<string>();
  let active: string | null = null;
  const warn = ports.warn ?? ((message: string): void => {
    // eslint-disable-next-line no-console
    console.warn(message);
  });

  const asking = (error: string | null, errorIsText = false, busy = false): UsernamePromptView => ({
    phase: "asking",
    busy,
    error,
    errorIsText,
  });

  return {
    view,
    async start(accountIdB64) {
      if (looked.has(accountIdB64)) return;
      looked.add(accountIdB64);
      active = accountIdB64;
      const token = ports.sessionToken();
      if (token === null) return;
      let candidates: readonly string[] = [];
      try {
        candidates = backupUsernameCandidates(await ports.backupCandidates(accountIdB64));
      } catch (err) {
        warn(`username from the backup unreadable: ${err instanceof Error ? err.name : "unknown"}`);
      }
      const resolution = await resolveFromBackup(ports, token, candidates);
      if (active !== accountIdB64) return;
      if (resolution.kind === "adopt") {
        await ports.adopt(resolution.username);
        return;
      }
      if (resolution.kind === "no-username") return;
      setView(asking(null));
    },
    async submit(typed) {
      const current = view();
      if (current.phase !== "asking" || current.busy) return;
      const token = ports.sessionToken();
      if (token === null) {
        setView({ phase: "hidden" });
        return;
      }
      setView(asking(null, false, true));
      const result = await checkUsername(ports, token, typed);
      if (view().phase !== "asking") return;
      switch (result.kind) {
        case "same":
          setView({ phase: "hidden" });
          await ports.adopt(result.username);
          return;
        case "none":
          setView({ phase: "hidden" });
          return;
        case "invalid":
          setView(asking(result.message, true));
          return;
        default:
          setView(asking(usernamePromptErrorKey(result.kind)));
      }
    },
    forgot() {
      if (view().phase === "asking") setView({ phase: "forgot" });
    },
    close() {
      setView({ phase: "hidden" });
    },
    reset() {
      looked.clear();
      active = null;
      setView({ phase: "hidden" });
    },
  };
}
