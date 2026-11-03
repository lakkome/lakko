// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0065 — the whole-session part of file lifetime, mounted once beside the
 * other session runtimes in `App.tsx`. Renders nothing.
 *
 *   * Installs the account-bound blob ports (`blob-access.ts`): the pass
 *     purchase (one `blob-upload` / `blob-read` anonymous token each) and the
 *     delete keys from the ACCOUNT identity's `VaultKeys`.
 *   * Drains the owed blob deletes at each landing and every few minutes.
 *   * The keep tick (section 3): while the app is in front, every
 *     {@link KEEP_TICK_MS}, the sources that are due (`keep-scheduler.ts`):
 *     the files of messages pinned (for everyone or for me) in each
 *     conversation this device holds, the saved messages' files, and the own
 *     profile photo.
 */
import { useGroupNames } from "../chat/GroupNameContext";
import { createEffect, onCleanup, untrack, type JSX } from "solid-js";

import { useAuth } from "../auth/AuthContext";
import { getDeviceKey } from "../auth/key-store";
import { base64Decode } from "../auth/account-id";
import { blobIdsOfMessages, bytesToHex } from "../chat/blob-ids";
import { useChats } from "../chat/ChatContext";
import { loadMessageLog } from "../chat/message-log-store";
import { usePins } from "../chat/PinContext";
import { useDeviceGroupSync } from "../devices/sync/DeviceGroupSyncContext";
import { b64ToBytes } from "../profile/avatar-model";
import { useProfiles } from "../profile/ProfileContext";
import {
  AnonTokenQuotaExhaustedError,
  anonTokenPoolEmpty,
  anonTokens,
  anonymousFetchInit,
} from "../tokens/anon-token-pool";
import { loadWasm } from "../wasm";
import {
  BlobPassCache,
  StorageLimitError,
  installBlobAccess,
  parseBlobPass,
  type BlobPassKind,
} from "./blob-access";
import { drainBlobDeletes } from "./blob-deletes";
import { blobDeleteVaultKeys } from "./blob-delete-keys";
import { deviceGroupLineageSeed } from "../devices/lifecycle";
import { DEFAULT_BASE_URL, StorageClient } from "./client";
import { markKept, parseKeepState, planKeeps, type KeepSource, type KeepState } from "./keep-scheduler";

/** How often the keep tick and the delete drain run while in front. */
export const KEEP_TICK_MS = 10 * 60_000;

async function buyPass(kind: BlobPassKind): Promise<{ pass: string; expiresAtMs: number }> {
  const context = kind === "upload" ? "blob-upload" : "blob-read";
  let response: Response;
  try {
    response = await anonTokens.spend(context, (name, value) =>
      fetch(`${DEFAULT_BASE_URL}/blob/pass/${kind}`, anonymousFetchInit({ method: "POST", headers: { [name]: value } })),
    );
  } catch (err) {
    // No token left for the period, or none until the next refill: the limit
    // for files is reached (UX 4.2).
    if (err instanceof AnonTokenQuotaExhaustedError || anonTokenPoolEmpty(err)) throw new StorageLimitError(kind);
    throw err;
  }
  if (!response.ok) throw new Error(`blob pass refused (${response.status})`);
  return parseBlobPass(await response.json(), kind);
}

function keepKey(accountId: string): string {
  return `lakko.blob-keeps.v1.${accountId}`;
}

function readKeepState(accountId: string): KeepState {
  try {
    return parseKeepState(localStorage.getItem(keepKey(accountId)));
  } catch {
    return parseKeepState(null);
  }
}

function writeKeepState(accountId: string, state: KeepState): void {
  try {
    localStorage.setItem(keepKey(accountId), JSON.stringify(state));
  } catch {
    // Storage blocked: the keeps run again sooner, which costs nothing.
  }
}

export function BlobAccessRuntime(): JSX.Element {
  const auth = useAuth();
  const chats = useChats();
  const groupNames = useGroupNames();
  const pins = usePins();
  const sync = useDeviceGroupSync();
  const profiles = useProfiles();

  const accountId = (): string | null => {
    const s = auth.state();
    return s.kind === "session" ? s.account.accountId : null;
  };

  // The ports, bound to the account identity.
  createEffect(() => {
    const identity = chats.accountIdentity();
    const account = accountId();
    if (identity === null || account === null) {
      installBlobAccess(null);
      return;
    }
    let keys: { blobDeletePublicKey(id: Uint8Array): Uint8Array; signBlobDelete(id: Uint8Array, m: string, p: string, b: Uint8Array): string; free(): void } | null = null;
    let headerName = "x-lakko-storage-proof";
    // ADR-0065 section 6 with ADR-0074: the delete keys come from the
    // account's LINEAGE seed (the one the DeviceGroup id is derived from), so
    // a file uploaded before a key rotation can still be deleted after it.
    const ready = (async () => {
      const w = await loadWasm();
      headerName = w.storageProofHeaderName();
      const deviceKey = await getDeviceKey(account);
      const lineage = deviceKey === null ? null : await deviceGroupLineageSeed(account, deviceKey);
      keys = blobDeleteVaultKeys(w, lineage, identity);
    })();
    installBlobAccess({
      passes: new BlobPassCache(buyPass),
      async deleteKey(blobId) {
        await ready;
        if (keys === null) throw new Error("vault keys not loaded");
        return keys.blobDeletePublicKey(blobId);
      },
      async signDelete(blobId, path) {
        await ready;
        if (keys === null) throw new Error("vault keys not loaded");
        return [headerName, keys.signBlobDelete(blobId, "DELETE", path, new Uint8Array(0))] as const;
      },
    });
    void ready.then(() => drainBlobDeletes(account));
    onCleanup(() => {
      installBlobAccess(null);
      void ready.then(() => {
        try {
          keys?.free();
        } catch {
          /* freed */
        }
        keys = null;
      });
    });
  });

  /** The keep sources this device can name now. */
  async function sources(account: string): Promise<KeepSource[]> {
    const out: KeepSource[] = [];
    const deviceKey = await getDeviceKey(account);
    const log = deviceKey === null ? null : await loadMessageLog(account, deviceKey);
    for (const g of untrack(chats.groups)) {
      const pinned = new Set<number>();
      for (const [id, p] of pins.shared(g.groupIdB64)) if (p.pinned) pinned.add(id);
      for (const id of pins.personal(g.groupIdB64).keys()) pinned.add(id);
      if (pinned.size === 0) continue;
      let hex: string;
      try {
        hex = bytesToHex(base64Decode(g.groupIdB64));
      } catch {
        continue;
      }
      out.push({ key: `pins:${hex}`, idsHex: blobIdsOfMessages(log?.groups[hex], pinned) });
    }
    out.push({ key: "saved", idsHex: untrack(sync.savedBlobIdsHex) });
    const avatar = untrack(profiles.ownProfile).avatar;
    if (avatar !== null) {
      const ids = [avatar.still.blobIdB64, avatar.video?.ref.blobIdB64].filter((b): b is string => b !== undefined);
      out.push({ key: "profile", idsHex: ids.map((b) => bytesToHex(b64ToBytes(b))) });
    }
    // ADR-0087 section 18.5 — the photos of the groups this account owns or
    // administers are kept by its devices, with no control anywhere.
    const photoIds: string[] = [];
    for (const g of untrack(chats.groups)) {
      const photo = groupNames.photoOf(g.groupIdB64);
      if (photo === undefined || photo.avatar === null) continue;
      if ((await groupNames.liveRole(g.group)) === "member") continue;
      for (const b of [photo.avatar.still.blobIdB64, photo.avatar.video?.ref.blobIdB64]) {
        if (b !== undefined) photoIds.push(bytesToHex(b64ToBytes(b)));
      }
    }
    if (photoIds.length > 0) out.push({ key: "conversation-photos", idsHex: photoIds });
    return out;
  }

  let ticking = false;
  async function tick(): Promise<void> {
    const account = accountId();
    if (ticking || account === null) return;
    if (typeof document !== "undefined" && document.visibilityState !== "visible") return;
    ticking = true;
    try {
      await drainBlobDeletes(account);
      const plan = planKeeps(readKeepState(account), await sources(account), Date.now());
      writeKeepState(account, plan.state);
      if (plan.keys.length === 0) return;
      const storage = new StorageClient();
      for (const batch of plan.batches) await storage.keepBlobs(batch);
      if (accountId() === account) writeKeepState(account, markKept(plan.state, plan.keys, Date.now()));
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("[storage] keep tick did not finish", err instanceof Error ? err.name : "");
    } finally {
      ticking = false;
    }
  }

  createEffect(() => {
    if (accountId() === null || chats.accountIdentity() === null) return;
    const first = setTimeout(() => void tick(), 60_000);
    const timer = setInterval(() => void tick(), KEEP_TICK_MS);
    onCleanup(() => {
      clearTimeout(first);
      clearInterval(timer);
    });
  });

  return null;
}
