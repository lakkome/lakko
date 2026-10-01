// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Mount-side MLS state rehydration (Phase 1.16e6c3) — the
 * closing piece of the persistence chain.
 *
 * 1.16e6c1 built the IDB snapshot store. 1.16e6c2 wired
 * auto-persist hooks at every state-mutating call so the
 * on-disk blob stays in sync with the canonical Identity's
 * provider. This module flips the flow on its head: take a
 * blob, hydrate the canonical Identity from it, and re-create
 * a `WasmGroup` reference for every `group_id` the snapshot
 * carries — so the active-groups registry comes back populated
 * after a page reload.
 *
 * ## When to call
 *
 * Only after `<ChatProvider>` has its canonical Identity set —
 * i.e. inside the post-OPAQUE branch of signup / login /
 * recover, **between** `chats.setIdentity(canonical)` and
 * `uploadIdentityBundle(...)`. Three properties matter for
 * that ordering:
 *
 *   1. The new Identity is fresh from `Identity.fromSeed(...)`
 *      — its provider is empty. `restoreStorage` therefore
 *      *replaces* nothing valuable and brings back the prior
 *      state the previous session persisted.
 *   2. The fresh KeyPackage minted by `uploadIdentityBundle`
 *      lands in the **already-restored** provider, so the new
 *      init/leaf private keys merge with the prior groups
 *      rather than wiping them out.
 *   3. `chats.persistSnapshot(...)` fires right after upload
 *      and captures the merged state.
 *
 * Per-flow expectations:
 *
 *   * **signup** — brand-new accountId, no prior snapshot.
 *     `loadMlsSnapshot` returns `null`, the helper bails out
 *     cleanly. Net effect: zero.
 *   * **login** — the same accountId on the same device. If a
 *     snapshot exists, `restoreStorage` brings back every
 *     active group; the loop below `Group::load`s each
 *     `group_id` and pushes it into the chat registry.
 *   * **recover** — the new password rotates the OPAQUE export
 *     key, which rotates the device root key, which makes any
 *     prior snapshot unreadable. `AuthContext.recover` deletes
 *     the snapshot row pre-flight (1.16e6c1), so this helper
 *     finds nothing on the rebuilt path.
 *
 * Failures are best-effort and surface as warnings — a corrupt
 * snapshot must not lock the user out of an authenticated
 * session. The user can always send a fresh ContactRequest /
 * accept a fresh Welcome to recover an active group.
 */
import { base64Decode, base64Encode } from "../auth/account-id";
import { getDeviceKey } from "../auth/key-store";
import { ensureGroupPushRouting } from "../push/group-routing";
import { loadWasm } from "../wasm";
import { ChatClient } from "./client";
import { type ChatApi } from "./ChatContext";
import { loadGroupNameRows } from "./group-name-idb";
import { GROUP_STAND_IN } from "./group-title";
import { loadMlsSnapshot } from "./mls-store";

/**
 * Pull the encrypted MLS snapshot for `accountIdB64`, decrypt
 * it under this device's root key, push it into the canonical
 * Identity's provider, and re-create a `WasmGroup` reference
 * for every `group_id` the snapshot carried.
 *
 * No-op (resolves successfully) when:
 *
 *   * the canonical Identity hasn't been set on `chats` yet
 *     (caller invariant violation — log + bail);
 *   * no device root key is on file for the account (caller
 *     hasn't completed the auth flow yet, or `forgetDevice`
 *     wiped both layers between calls);
 *   * no snapshot row exists for the account (first signup
 *     ever / post-recover / never-snapshotted device).
 *
 * Per-group failures (corrupt `group_id`, OpenMLS rejection,
 * dangling id pointing at a row the storage doesn't actually
 * carry) are logged and skipped — other groups in the same
 * snapshot still come up. We deliberately do not throw: this
 * is a "best-effort restore", not a hard precondition.
 */
export async function rehydrateMlsState(
  chats: ChatApi,
  accountIdB64: string,
): Promise<void> {
  const canonical = chats.identity();
  if (canonical === null) {
    // Caller invariant: canonical Identity must have been set
    // before this call. If it isn't, we'd `Group.load` against
    // a non-existent provider. Surface as a warning rather
    // than throwing — the auth flow will keep running and the
    // user just won't see prior groups until next login.
    // eslint-disable-next-line no-console
    console.warn(
      "rehydrateMlsState: no canonical Identity set; skipping",
    );
    return;
  }

  let deviceKey: CryptoKey | null;
  try {
    deviceKey = await getDeviceKey(accountIdB64);
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn("rehydrateMlsState: getDeviceKey failed", err);
    return;
  }
  if (deviceKey === null) {
    // No device key → nothing to decrypt with. Caller is in a
    // half-reset state (forgetDevice in flight, IDB quota
    // exhaustion). Drop and let the user re-auth.
    return;
  }

  let snapshot;
  try {
    snapshot = await loadMlsSnapshot(accountIdB64, deviceKey);
  } catch (err) {
    // Decrypt failure (key mismatch / tampered ciphertext) or
    // IDB read error. The snapshot is unrecoverable at this
    // point; logging + bailing is the only safe option.
    // eslint-disable-next-line no-console
    console.warn("rehydrateMlsState: loadMlsSnapshot failed", err);
    return;
  }
  if (snapshot === null) {
    // First signup, or post-recover after the snapshot was
    // explicitly deleted, or a device that's never been
    // snapshotted. Either way: nothing to do.
    return;
  }

  // ADR-0050 — a snapshot taken under another leaf identity (every
  // pre-ADR-0050 snapshot was taken under the ACCOUNT identity; the chats now
  // run on the DEVICE identity) must not be restored: the groups in it were
  // signed by a key this identity does not hold, and `restoreStorage`
  // replaces the whole provider. Start empty instead; the next persist
  // overwrites the row under the right key.
  const ownLeafB64 = base64Encode(canonical.signaturePublicKey);
  if (snapshot.leafKeyB64 !== ownLeafB64) {
    // eslint-disable-next-line no-console
    console.warn(
      "rehydrateMlsState: snapshot belongs to another leaf identity; starting empty",
    );
    return;
  }

  try {
    canonical.restoreStorage(snapshot.snapshotBytes);
  } catch (err) {
    // Corrupt snapshot blob — the WASM Identity rejected the
    // restore. The provider is in a partially-mutated state;
    // log and bail. The user keeps a working canonical (the
    // freshly-built one) but loses prior groups for this
    // session. Persisting again at the end of the auth flow
    // overwrites the bad blob with a clean one.
    // eslint-disable-next-line no-console
    console.warn("rehydrateMlsState: restoreStorage failed", err);
    return;
  }

  // ADR-0042 — a ChatClient dedicated to the legacy-sweep broadcast: it
  // posts a push-routing-secret distribution envelope as an E2EE MLS
  // application message (persist-before-publish). Wired to the canonical
  // Identity + the snapshot persist, same as the chat view's client.
  const sweepClient = new ChatClient(undefined, {
    identityProvider: () => chats.identity(),
    onPersist: async () => {
      await chats.persistSnapshot(accountIdB64);
    },
  });

  // ADR-0051 — the shared names this device holds, so a restored group is
  // titled at once rather than from an id. Best-effort: with no row (or an
  // unreadable one) every group gets the unnamed stand-in and the title
  // resolver builds it from the members.
  let names: ReadonlyMap<string, { readonly name: string }> | null = null;
  try {
    names = await loadGroupNameRows(accountIdB64, deviceKey);
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn("rehydrateMlsState: group-name rows unreadable", err);
  }

  // Re-create a WasmGroup for every persisted id. Group.load
  // is a static factory that walks the just-restored provider's
  // storage and rebuilds the in-memory `MlsGroup` struct
  // (1.16e6a / 1.16e6b). Per-group failures are isolated so a
  // single bad id doesn't take down the whole list.
  const wasm = await loadWasm();
  for (const groupIdB64 of snapshot.knownGroupIds) {
    let groupIdBytes: Uint8Array;
    try {
      groupIdBytes = base64Decode(groupIdB64);
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn(
        "rehydrateMlsState: bad groupIdB64 in snapshot",
        groupIdB64,
        err,
      );
      continue;
    }

    let group;
    try {
      group = wasm.Group.load(canonical, groupIdBytes);
    } catch (err) {
      // Group.load returns Err only on actual OpenMLS rejection
      // (corrupt provider state for this id). The Option<Group>
      // surfaces as `undefined` when there's just no row — see
      // `Group.load(...)` in `lakko_ffi_wasm.d.ts`.
      // eslint-disable-next-line no-console
      console.warn(
        "rehydrateMlsState: Group.load threw for",
        groupIdB64,
        err,
      );
      continue;
    }
    if (group === undefined) {
      // Snapshot's `knownGroupIds` listed this id but the
      // restored storage doesn't actually carry a row for it.
      // Inconsistent state (a prior persist captured the list
      // before the storage was fully written, or the user
      // explicitly removed the group between snapshots and the
      // list wasn't re-pruned). Skip — the next persist will
      // drop the dangling id.
      // eslint-disable-next-line no-console
      console.warn(
        "rehydrateMlsState: Group.load returned undefined for",
        groupIdB64,
      );
      continue;
    }

    chats.add({
      label: restoredGroupLabel(names, groupIdB64),
      group,
    });

    // ADR-0042 legacy-sweep — provision opaque push routing for every
    // restored group. Owner + no secret yet → mint + persist + distribute
    // the envelope + subscribe; a member holding a secret → subscribe under
    // `push_gid`; anyone else → defer (no push under `group_id`). Idempotent
    // + race-safe (shared in-flight lock with the chat view's onMount).
    // Best-effort — a failure never blocks the session restore.
    try {
      await ensureGroupPushRouting(accountIdB64, group, canonical, (bytes) =>
        sweepClient
          .broadcastControl(null, group, bytes)
          .then(() => undefined),
      );
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("push-routing legacy-sweep failed for", groupIdB64, err);
    }
  }
}

/**
 * Label for a rehydrated group: the shared name this device holds for it
 * (ADR-0051), else the bare stand-in `Group`, which `group-title.ts` reads
 * as "unnamed" and the title resolver replaces with the members' names. An
 * id prefix is never shown (docs/ux/client-redesign.md section 5.1).
 */
function restoredGroupLabel(
  names: ReadonlyMap<string, { readonly name: string }> | null,
  groupIdB64: string,
): string {
  const held = names?.get(groupIdB64)?.name ?? "";
  return held.length > 0 ? held : GROUP_STAND_IN;
}
