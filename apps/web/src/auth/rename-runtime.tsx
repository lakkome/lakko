// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Changing a username (ADR-0076): the app's wiring of the one resumable
 * operation in `username-change.ts`, used by Settings > Profile
 * (docs/ux/profile-screen.md section 4), and the landing's resume of a
 * rename that was not finished.
 *
 * After a change the profile, the Everyone record and name terms, the
 * introduction offer and the channel member cards are published again by
 * their own publishers: their inputs (the name and its certificate) changed.
 */
import { createEffect, type JSX } from "solid-js";

import { useAuth } from "./AuthContext";
import { handleBindLimitedMessage } from "./auth-error-copy";
import { AuthClient, AuthHttpError } from "./client";
import { loginHandleFor } from "./login-handle";
import { heldAddress } from "./protect/address-register";
import { accountFactors, removingLeavesNone } from "./protect/factors";
import { plainStore } from "./protect/local";
import { protectionStatus } from "./protect/protection-state";
import {
  readRenameOwed,
  renameEmail,
  renameNeedsAddress,
  renameResume,
  writeRenameOwed,
  type Answer,
  type RenameFailure,
  type RenamePorts,
} from "./username-change";
import { DiscoveryClient, DiscoveryHttpError } from "../discovery/client";
import { t } from "../l10n";
import { useProfiles } from "../profile/ProfileContext";

/**
 * The recovery email a rename carries for this account, or `null` (none, or
 * this device does not hold the address and none was typed: the server then
 * drops it, or refuses the change when it is the account's last second way
 * in). `typedAddress` is what the person typed on the username screen when
 * {@link renameAsksForAddress} said to ask (UX 19.1c).
 */
export function renameEmailFor(accountIdB64: string, typedAddress?: string): { address: string } | null {
  const status = protectionStatus(accountIdB64);
  if (status?.enabled !== true) return null;
  const held = heldAddress(accountIdB64);
  if (held !== null) return { address: held };
  const typed = typedAddress?.trim() ?? "";
  return typed.length > 0 ? { address: typed } : null;
}

/**
 * Reactive: whether the username screen asks for the recovery email before
 * the change, with the line `protect.username_email` (UX 19.1c): the email
 * is the account's last second way in and this device holds no address.
 */
export function renameAsksForAddress(accountIdB64: string | null): boolean {
  if (accountIdB64 === null) return false;
  const status = protectionStatus(accountIdB64);
  const factors = accountFactors(accountIdB64);
  return renameNeedsAddress(
    renameEmail(status?.enabled === true, heldAddress(accountIdB64) !== null),
    factors !== null && removingLeavesNone(factors, "email"),
  );
}

/** What the rename needs from the app, for one account. Also used by
 *  Settings > Profile, which runs the same operation from its Save. */
export function useRenamePorts(): (account: { accountIdB64: string; accountId: Uint8Array; token: string }) => RenamePorts {
  const auth = useAuth();
  const profiles = useProfiles();
  return (account) => ({
    handleFor: (username) => loginHandleFor(username),
    bind: async (args): Promise<Answer | null> => {
      try {
        await new AuthClient().bindLoginHandle(account.token, args.handle, {
          freshGrant: args.grant,
          previousHandle: args.previousHandle,
          ...(args.email === null ? {} : { recoveryEmail: { address: args.email.address } }),
        });
        return null;
      } catch (err) {
        if (err instanceof AuthHttpError) {
          return { status: err.status, errorCode: err.errorCode, retryAfterSecs: err.retryAfter };
        }
        return { status: null, errorCode: null, retryAfterSecs: null };
      }
    },
    replace: async (username) => {
      try {
        const discovery = new DiscoveryClient();
        discovery.setSessionToken(account.token);
        const done = await discovery.replaceUsername(account.accountId, username);
        return { ok: true, username: done.normalisedUsername, certificate: done.usernameCertificate };
      } catch (err) {
        if (err instanceof DiscoveryHttpError) {
          return {
            ok: false,
            answer: { status: err.status, errorCode: err.errorCode, retryAfterSecs: null },
          };
        }
        return { ok: false, answer: { status: null, errorCode: null, retryAfterSecs: null } };
      }
    },
    saveOwed: (record) => writeRenameOwed(plainStore(), account.accountIdB64, record),
    finishLocal: async (username, certificate) => {
      await auth.applyRenamedUsername(username);
      try {
        await profiles.usernameClaimed(certificate);
      } catch {
        // The publishers retry by themselves.
      }
    },
  });
}

/** Whole-session: finish or drop a rename a landing finds unfinished (a
 *  crash, a lost answer). One whose rollback is owed waits for the person,
 *  who finishes it from Settings > Profile. Renders nothing. */
export function RenameResume(): JSX.Element {
  const auth = useAuth();
  const portsFor = useRenamePorts();
  const account = (): { accountIdB64: string; accountId: Uint8Array; token: string } | null => {
    const s = auth.state();
    const secrets = auth.identitySecrets();
    if (s.kind !== "session" || secrets === null || auth.signedOut() !== null) return null;
    return { accountIdB64: s.account.accountId, accountId: secrets.accountId, token: secrets.sessionToken };
  };

  // A rename that was not finished (a crash, a lost answer) is finished or
  // dropped at the landing; one whose rollback is owed waits for the person.
  let resumedFor: string | null = null;
  async function resume(acct: { accountIdB64: string; accountId: Uint8Array; token: string }): Promise<void> {
    const record = readRenameOwed(plainStore(), acct.accountIdB64);
    if (record === null) return;
    const ports = portsFor(acct);
    let state: "same" | "other" | "none" | null = null;
    if (record.phase === "binding") {
      try {
        state = await new AuthClient().checkLoginHandle(acct.token, (await loginHandleFor(record.newUsername)).wire);
      } catch {
        state = null;
      }
    }
    switch (renameResume(record, state)) {
      case "finish-local":
        await ports.finishLocal(record.newUsername, null);
        ports.saveOwed(null);
        return;
      case "drop":
        ports.saveOwed(null);
        return;
      case "replace-again": {
        const replaced = await ports.replace(record.newUsername);
        if (replaced.ok) {
          ports.saveOwed({ ...record, newUsername: replaced.username, phase: "replaced" });
          await ports.finishLocal(replaced.username, replaced.certificate);
          ports.saveOwed(null);
        } else if (replaced.answer.status !== null) {
          // Refused: the old handle is to be bound back, which needs the
          // person's proof.
          ports.saveOwed({ ...record, phase: "rollback" });
        }
        return;
      }
      case "wait-for-person":
      case "later":
        return;
    }
  }
  createEffect(() => {
    const acct = account();
    if (acct === null) {
      resumedFor = null;
      return;
    }
    if (resumedFor === acct.accountIdB64) return;
    resumedFor = acct.accountIdB64;
    void resume(acct).catch(() => undefined);
  });

  return null;
}

export function failureText(failure: RenameFailure): string | null {
  switch (failure.kind) {
    case "taken":
      return t("auth.signup.error.username_taken");
    case "bind-limited":
      return handleBindLimitedMessage(failure.retryAfterSecs);
    case "offline":
      return t("common.error.offline");
    case "last-factor":
      // The change would drop the account's last second way in: the screen
      // asks for the recovery email (UX 19.1c).
      return t("protect.username_email");
    case "address-mismatch":
    case "error":
      return t("recovery_phrase.fresh.error.generic");
    case "proof-required":
      return null;
  }
}

