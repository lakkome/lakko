// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Recovery from another device (ADR-0018 factor (a), the no-password entry):
 * the user types only the username; a device that is still logged in
 * confirms the request, and the identity plus a single-use access grant
 * arrive end-to-end encrypted. The grant redeems for a session, so neither a
 * password nor the phrase is typed here.
 */
import type { JSX } from "solid-js";
import { useAuth } from "../AuthContext";
import { base64Encode } from "../account-id";
import { requireValidUsername } from "../../profile/username-field";
import { DiscoveryClient } from "../../discovery/client";
import {
  prepareAnonymousJoinFlow,
  type AnonymousJoinSession,
  type JoinRecoveryOutcome,
} from "../../devices/orchestrator";
import RestoreViaDevicePanel, {
  type DeviceRestoreFlowHandle,
} from "../../devices/RestoreViaDevicePanel";
import { UsernameField } from "./UsernameField";

export function DeviceRecover(props: {
  readonly username: () => string;
  readonly setUsername: (v: string) => void;
  readonly onDone: () => void;
}): JSX.Element {
  const auth = useAuth();
  const discovery = new DiscoveryClient();

  // Captured across prepare -> onRestored (the panel guarantees the ordering;
  // components run once in Solid, so plain variables work).
  let resolved: {
    accountIdBytes: Uint8Array;
    accountIdB64: string;
    username: string;
  } | null = null;
  let liveSession: AnonymousJoinSession | null = null;

  async function prepare(): Promise<DeviceRestoreFlowHandle> {
    const trimmed = props.username().trim();
    if (trimmed.length === 0) {
      throw new Error("Enter the username of the account.");
    }
    // ADR-0048 section 4: throws the reason copy; no lookup is sent.
    const lookup = await discovery.lookup(await requireValidUsername(trimmed));
    if (lookup.account === null) {
      throw new Error("No account with that username. Check the spelling.");
    }
    const accountIdBytes = lookup.account;
    const accountIdB64 = base64Encode(accountIdBytes);
    resolved = {
      accountIdBytes,
      accountIdB64,
      username: lookup.normalisedUsername,
    };
    // No device key exists yet, so the join runs with an in-memory device
    // seed until `completeFromDeviceRestore` bootstraps the key and
    // `persistTo` lands the state.
    const session = await prepareAnonymousJoinFlow({
      accountIdBytes,
      accountIdB64,
    });
    liveSession = session;
    return {
      qrPayload: session.qrPayload,
      awaitWelcome: (signal) => session.awaitWelcome(signal),
      accept: async (welcome, welcomeCursor) => {
        const outcome = await session.accept(welcome, { welcomeCursor });
        return outcome.recovery;
      },
      dispose: () => {
        session.dispose();
        liveSession = null;
      },
    };
  }

  async function onRestored(outcome: JoinRecoveryOutcome): Promise<void> {
    if (resolved === null) {
      outcome.phraseSeed.fill(0);
      throw new Error("internal: no resolved account for restore");
    }
    // The redeemed session token is the ONLY access on this path; there is
    // no password. Without it the restore cannot complete.
    if (outcome.sessionToken === null || outcome.sessionExpiresAtMs === null) {
      outcome.phraseSeed.fill(0);
      throw new Error(
        "Your other device sent the identity, but this device could not be let in. Try again, or use your recovery phrase.",
      );
    }
    const session = liveSession;
    await auth.completeFromDeviceRestore({
      accountIdBytes: resolved.accountIdBytes,
      accountIdB64: resolved.accountIdB64,
      username: resolved.username,
      phraseSeed: outcome.phraseSeed,
      sessionToken: outcome.sessionToken,
      sessionExpiresAtMs: outcome.sessionExpiresAtMs,
      ...(session !== null
        ? {
            persistJoinState: (deviceKey: CryptoKey): Promise<void> => session.persistTo(deviceKey),
          }
        : {}),
    });
    props.onDone();
  }

  return (
    <div class="lk-stack">
      <p class="lk-muted">
        Enter your username, then confirm on a device that is still logged in and compare
        the code it shows. Nothing is sent that Lakko's servers can read. No camera needed; a
        QR code is there if you prefer.
      </p>
      <div>
        <UsernameField value={props.username} setValue={props.setUsername} />
      </div>
      <RestoreViaDevicePanel
        prepare={prepare}
        onRestored={onRestored}
        requestAccount={() => resolved?.accountIdBytes ?? null}
      />
    </div>
  );
}
