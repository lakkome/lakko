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
import { base64Encode } from "./account-id";
import { handleBindLimitedMessage } from "./auth-error-copy";
import { AuthClient, AuthHttpError } from "./client";
import { loginHandleFor } from "./login-handle";
import { heldAddress } from "./protect/address-register";
import { runEmailVault } from "./protect/email-vault";
import { plainStore } from "./protect/local";
import { protectionStatus } from "./protect/protection-state";
import { RESEAL_SALT_LEN } from "./protect/reseal";
import {
  readRenameOwed,
  renameEmail,
  renameResume,
  renameSealsVault,
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
 * this device does not hold the address: the server then drops it).
 */
export function renameEmailFor(accountIdB64: string): { address: string; sealVault: boolean } | null {
  const status = protectionStatus(accountIdB64);
  const held = heldAddress(accountIdB64);
  if (renameEmail(status?.enabled === true, held !== null) !== "kept" || held === null) return null;
  return { address: held, sealVault: renameSealsVault("kept", status?.phraseSaved === true) };
}

/** What the rename needs from the app, for one account. Also used by
 *  Settings > Profile, which runs the same operation from its Save. */
export function useRenamePorts(): (account: { accountIdB64: string; accountId: Uint8Array; token: string }) => RenamePorts {
  const auth = useAuth();
  const profiles = useProfiles();
  return (account) => ({
    handleFor: (username) => loginHandleFor(username),
    sealVault: async (handle, address) => {
      const phrase = await auth.devicePhrase();
      if (phrase === null) return null;
      const salt = crypto.getRandomValues(new Uint8Array(RESEAL_SALT_LEN));
      const sealed = await runEmailVault({ kind: "seal", phrase, loginHandle: handle, address, vaultSalt: salt });
      return sealed.kind === "sealed" ? { salt, sealed: sealed.sealed } : null;
    },
    bind: async (args): Promise<Answer | null> => {
      try {
        await new AuthClient().bindLoginHandle(account.token, args.handle, {
          freshGrant: args.grant,
          previousHandle: args.previousHandle,
          ...(args.email === null
            ? {}
            : {
                recoveryEmail: {
                  address: args.email.address,
                  ...(args.email.vault === undefined
                    ? {}
                    : {
                        vaultSalt: base64Encode(args.email.vault.salt),
                        sealedEntropy: base64Encode(args.email.vault.sealed),
                      }),
                },
              }),
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
    case "address-mismatch":
    case "error":
      return t("recovery_phrase.fresh.error.generic");
    case "proof-required":
      return null;
  }
}

