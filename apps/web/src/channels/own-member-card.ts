// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0060 section 3 - keep this device's member card current while a
 * channel view is open.
 *
 * Re-runs when the channel's epoch, its seed, the own profile or the "who
 * can find me" setting changes; `ensureOwnMemberCard` decides whether an
 * upload is actually due (a new epoch, or different content). A card the
 * channel refuses because it does not know the key re-registers the key once
 * and tries again.
 */
import { createEffect, on, onCleanup } from "solid-js";

import { base64Decode } from "../auth/account-id";
import { useAuth } from "../auth/AuthContext";
import { useProfiles } from "../profile/ProfileContext";
import { visibilitySetting } from "../reach/visibility-store";
import { subscribe } from "./api";
import { bytesToHex, type ChannelSubscriberKeys } from "./composer";
import { ensureOwnMemberCard } from "./member-cards";

export interface OwnCardSource {
  readonly channelIdHex: () => string;
  /** The registered subscriber key pair, or `undefined` while there is none. */
  readonly keys: () => ChannelSubscriberKeys | undefined;
  /** The channel's current epoch, `undefined` until fetched. */
  readonly epoch: () => number | undefined;
  /** The seed of `epoch()`, `undefined` when this device does not hold it. */
  readonly seed: () => Uint8Array | undefined;
  /** `false` while the view is not in a state to upload (still subscribing). */
  readonly enabled: () => boolean;
}

function accountIdBytes(b64: string): Uint8Array | null {
  try {
    const bytes = base64Decode(b64);
    return bytes.length === 16 ? bytes : null;
  } catch {
    return null;
  }
}

export function useOwnMemberCard(source: OwnCardSource): void {
  const auth = useAuth();
  const profiles = useProfiles();
  let alive = true;
  onCleanup(() => {
    alive = false;
  });

  createEffect(
    on(
      () =>
        [
          source.enabled(),
          source.channelIdHex(),
          source.keys(),
          source.epoch(),
          source.seed(),
          profiles.ownProfile(),
          profiles.ownUsername(),
          visibilitySetting(),
        ] as const,
      ([enabled, channelIdHex, keys, epoch, seed, own, username]) => {
        if (!enabled || keys === undefined || epoch === undefined || seed === undefined) return;
        const s = auth.state();
        const accountId = s.kind === "session" ? accountIdBytes(s.account.accountId) : null;
        const input = {
          channelIdHex,
          keys,
          epoch,
          seed,
          displayName: own.displayName,
          username,
          updatedAtMs: own.updatedAtMs,
          accountId,
          usernameCertificate: profiles.ownUsernameCertificate(),
        };
        void (async (): Promise<void> => {
          const outcome = await ensureOwnMemberCard(input);
          if (outcome !== "not-registered" || !alive) return;
          try {
            await subscribe(channelIdHex, bytesToHex(keys.public));
          } catch {
            return;
          }
          if (alive) await ensureOwnMemberCard(input);
        })();
      },
    ),
  );
}
