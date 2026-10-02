// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Recovery from another device (ADR-0018 factor (a), the no-password entry):
 * the user types only the username; a device that is still logged in
 * confirms the request, and the recovery phrase (as its entropy, recovery
 * payload v2) plus a single-use access grant arrive end-to-end encrypted.
 * This device then holds the keys and the phrase, like the one that
 * confirmed it. The grant redeems for a session, so neither a
 * password nor the phrase is typed here.
 *
 * ADR-0056: nothing here looks the name up before the redeem. The typed
 * username becomes its login handle locally; the pairing payload and the
 * device request carry the handle, and the account id is learned from the
 * grant redeem, after the other device confirmed. `completeFromDeviceRestore`
 * then looks the typed name up at discovery with the redeemed session and
 * refuses, persisting nothing, unless it resolves to that same account (a
 * squatted login handle at auth must not land this device in another
 * account under the typed name).
 */
import type { JSX } from "solid-js";
import { useAuth } from "../AuthContext";
import { base64Encode } from "../account-id";
import { nameTargetFor } from "../login-handle";
import {
  prepareAnonymousJoinFlow,
  type AnonymousJoinSession,
  type JoinRecoveryOutcome,
} from "../../devices/orchestrator";
import RestoreViaDevicePanel, {
  type DeviceRestoreFlowHandle,
} from "../../devices/RestoreViaDevicePanel";
import { UsernameField } from "./UsernameField";
import { t } from "../../l10n";
import { KeyedError } from "../../l10n/keyed-error";

export function DeviceRecover(props: {
  readonly username: () => string;
  readonly setUsername: (v: string) => void;
  readonly onDone: () => void;
}): JSX.Element {
  const auth = useAuth();

  // Captured across prepare -> onRestored (the panel guarantees the ordering;
  // components run once in Solid, so plain variables work).
  let resolved: {
    loginHandle: Uint8Array;
    username: string;
  } | null = null;
  let liveSession: AnonymousJoinSession | null = null;

  async function prepare(): Promise<DeviceRestoreFlowHandle> {
    const trimmed = props.username().trim();
    if (trimmed.length === 0) {
      throw new KeyedError("recovery.username_required");
    }
    // ADR-0048 section 4 / ADR-0056: a name the rule refuses throws the
    // reason copy; nothing is sent. An unknown name is not detectable here
    // by design: the request simply goes unanswered.
    const target = await nameTargetFor(trimmed);
    resolved = { loginHandle: target.handle.bytes, username: target.username };
    // No device key exists yet, so the join runs with an in-memory device
    // seed until `completeFromDeviceRestore` bootstraps the key and
    // `persistTo` lands the state.
    const session = await prepareAnonymousJoinFlow({
      loginHandle: target.handle.bytes,
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
      outcome.phraseEntropy.fill(0);
      throw new Error("internal: no resolved account for restore");
    }
    // The redeemed session token is the ONLY access on this path; there is
    // no password. Without it the restore cannot complete.
    if (
      outcome.sessionToken === null ||
      outcome.sessionExpiresAtMs === null ||
      outcome.accountIdBytes === null
    ) {
      outcome.phraseEntropy.fill(0);
      throw new KeyedError("device_link.error.not_let_in");
    }
    const session = liveSession;
    // ADR-0056: the account id comes from the grant redeem.
    const accountIdBytes = outcome.accountIdBytes;
    await auth.completeFromDeviceRestore({
      accountIdBytes,
      accountIdB64: base64Encode(accountIdBytes),
      username: resolved.username,
      phraseEntropy: outcome.phraseEntropy,
      sessionToken: outcome.sessionToken,
      sessionExpiresAtMs: outcome.sessionExpiresAtMs,
      // This form is reached through "Forgot password?": offer the
      // new-password step once the session lands.
      forgotPassword: true,
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
      <p class="lk-muted">{t("recovery.device.intro")}</p>
      <div>
        <UsernameField value={props.username} setValue={props.setUsername} />
      </div>
      <RestoreViaDevicePanel
        prepare={prepare}
        onRestored={onRestored}
        requestHandle={() => resolved?.loginHandle ?? null}
      />
    </div>
  );
}
