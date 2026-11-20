// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The server-side cleanups of an account deletion on this device
 * ({@link runAccountDeletion}): built after the auth deletion succeeded and
 * before the wipe, while the route rows and the session token still exist.
 *
 * - One per push-gw subscription this device holds for the account
 *   (`POST /push/unsubscribe {sub_id}`, ADR-0078 phase 3). push-gw cannot
 *   tie a subscription to an account, so only this device can remove these.
 *   The request carries no session; it spends a `push-register` anonymous
 *   token from the pool. The pool cannot be refilled once the auth account
 *   is gone, so this removes as many subscriptions as the pool holds tokens
 *   for. A removed subscription's route row is deleted here, so the wipe's
 *   own unsubscribe does not spend another token on it.
 * - The backup blob (`DELETE /backup`). Storage verifies the session token
 *   statelessly, so it works for the token's remaining lifetime.
 * - The directory entry (`DELETE /discovery/account`).
 */

import { DiscoveryClient } from "../discovery/client";
import { PushClient } from "../push/client";
import { deleteRoute, listRoutesForAccount } from "../push/group-routing-store";
import { StorageClient } from "../storage/client";
import {
  anonTokenPoolEmpty,
  anonTokenQuotaExhausted,
  anonTokens,
} from "../tokens/anon-token-pool";
import { retryableHttpFailure, type DeletionCleanup } from "./account-deletion";

/** The issuer's daily `push-register` quota: asking for more is pointless. */
const PUSH_REGISTER_DAILY_QUOTA = 400;

const SUB_ID_HEX = /^[0-9a-f]{32}$/;

/** How many `push-register` tokens a deletion will spend: one per push-gw
 *  subscription this device holds for the account. */
export function pushUnsubscribeCount(routes: readonly { subIdHex?: string }[]): number {
  return Math.min(
    PUSH_REGISTER_DAILY_QUOTA,
    routes.filter((r) => r.subIdHex !== undefined && SUB_ID_HEX.test(r.subIdHex)).length,
  );
}

/** The port {@link prepareDeletionTokens} needs from the token pool. */
export interface PushTokenPool {
  ensureAtLeast(context: "push-register", count: number): Promise<number>;
}

/**
 * Before the auth deletion: hold one `push-register` token per subscription
 * the cleanups will remove (auth issues none afterwards). Best-effort: fewer
 * tokens than asked are logged by count only, and the deletion goes on.
 */
export async function prepareDeletionTokens(
  accountIdB64: string,
  deps: {
    readonly listRoutes?: (accountIdB64: string) => Promise<readonly { subIdHex?: string }[]>;
    readonly pool?: PushTokenPool;
  } = {},
): Promise<void> {
  const routes = await (deps.listRoutes ?? listRoutesForAccount)(accountIdB64);
  const want = pushUnsubscribeCount(routes);
  if (want === 0) return;
  const held = await (deps.pool ?? anonTokens).ensureAtLeast("push-register", want);
  if (held < want) {
    console.warn("[account] delete: fewer push tokens than subscriptions", held, want);
  }
}

function fromHex(hex: string): Uint8Array {
  const out = new Uint8Array(hex.length / 2);
  for (let i = 0; i < out.length; i++) out[i] = Number.parseInt(hex.slice(2 * i, 2 * i + 2), 16);
  return out;
}

/** A push unsubscribe a retry cannot help: no token left, or a 4xx. */
function pushRetryable(err: unknown): boolean {
  if (anonTokenPoolEmpty(err) || anonTokenQuotaExhausted(err) !== null) return false;
  return retryableHttpFailure(err);
}

export async function buildDeletionCleanups(
  accountIdB64: string,
  sessionToken: string,
): Promise<DeletionCleanup[]> {
  const cleanups: DeletionCleanup[] = [];
  let routes: Awaited<ReturnType<typeof listRoutesForAccount>> = [];
  try {
    routes = await listRoutesForAccount(accountIdB64);
  } catch (err) {
    console.warn("[account] delete: push routes unreadable", err instanceof Error ? err.name : "unknown");
  }
  for (const route of routes) {
    const subIdHex = route.subIdHex;
    if (subIdHex === undefined || !SUB_ID_HEX.test(subIdHex)) continue;
    const pushGidHex = route.pushGidHex;
    cleanups.push({
      label: "push-unsubscribe",
      run: async () => {
        await new PushClient().unsubscribe(fromHex(subIdHex));
        await deleteRoute(accountIdB64, pushGidHex).catch(() => undefined);
      },
      retryable: pushRetryable,
    });
  }
  cleanups.push({
    label: "backup",
    run: async () => {
      const storage = new StorageClient();
      storage.setSessionToken(sessionToken);
      await storage.deleteBackup();
    },
    retryable: retryableHttpFailure,
  });
  cleanups.push({
    label: "discovery",
    run: async () => {
      const d = new DiscoveryClient();
      d.setSessionToken(sessionToken);
      await d.deleteAccount();
    },
    retryable: retryableHttpFailure,
  });
  return cleanups;
}
