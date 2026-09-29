// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0060 section 7 / UX `who-can-find-me.md` - the "who can find me"
 * setting of the signed-in account.
 *
 * The setting is a wasm `VisibilitySetting`; every change goes through a
 * `lakko_ui_rules` transition (`chooseVisibilityMode`, `setContactsOfContacts`,
 * ... - the Settings screen calls them), is stamped with a strictly
 * increasing `updatedAtMs` and persisted encrypted as its Visibility envelope
 * bytes (`composeVisibilityEnvelope`), so the persisted form is exactly the
 * form another device would receive. After a change the reach session syncs:
 * keys are registered or removed and `open` is set.
 *
 * Cross-device sync of the envelope over the DeviceGroup is NOT built on web:
 * the wasm DeviceGroup composes only its four typed payloads and has no way
 * to carry the Visibility envelope. {@link receiveVisibility} is the intake a
 * carrier would call; it applies the ADR-0044 recency rule
 * (`visibilityIsNewer`).
 */
import { createSignal, type Accessor } from "solid-js";

import type { VisibilitySetting } from "../../wasm/lakko_ffi_wasm";
import { base64Decode, base64Encode } from "../auth/account-id";
import { loadWasm } from "../wasm";
import { loadReachRow, saveReachRow } from "./reach-store";

const [setting, setSetting] = createSignal<VisibilitySetting | null>(null, { equals: false });
let boundAccount: string | null = null;

/** Reactive: the current setting, `null` while signed out or loading. */
export const visibilitySetting: Accessor<VisibilitySetting | null> = setting;

/** The stamp a change gets: strictly after the previous one even when the
 *  clock stands still or went backwards. */
export function nextStamp(previousMs: bigint, nowMs: number): bigint {
  const now = BigInt(Math.max(0, Math.floor(nowMs)));
  return now > previousMs ? now : previousMs + 1n;
}

/** Load the account's setting (the default for a new account) and publish
 *  it. Returns the setting. */
export async function loadVisibility(accountIdB64: string): Promise<VisibilitySetting> {
  const wasm = await loadWasm();
  boundAccount = accountIdB64;
  const row = await loadReachRow(accountIdB64, "visibility");
  let loaded: VisibilitySetting | null = null;
  if (row !== null) {
    try {
      loaded = wasm.decodeVisibilityEnvelope(base64Decode(row));
    } catch {
      loaded = null;
    }
  }
  const value = loaded ?? wasm.defaultVisibilitySetting();
  if (boundAccount === accountIdB64) setSetting(value);
  if (loaded === null) await persist(accountIdB64, value);
  return value;
}

/** Sign-out. */
export function clearVisibility(): void {
  boundAccount = null;
  setSetting(null);
}

async function persist(accountIdB64: string, value: VisibilitySetting): Promise<void> {
  const wasm = await loadWasm();
  await saveReachRow(accountIdB64, "visibility", base64Encode(wasm.composeVisibilityEnvelope(value)));
}

/**
 * Apply one transition to the current setting, stamp it, persist it and
 * publish it. `transition` is a `lakko_ui_rules` rule bound to its argument
 * (`(s) => wasm.chooseVisibilityMode(s, mode)`). Returns the new setting, or
 * `null` when there is no setting loaded.
 */
export async function updateVisibility(
  transition: (current: VisibilitySetting) => VisibilitySetting,
): Promise<VisibilitySetting | null> {
  const account = boundAccount;
  const current = setting();
  if (account === null || current === null) return null;
  const moved = transition(current);
  const stamped = moved.withUpdatedAtMs(nextStamp(current.updatedAtMs, Date.now()));
  moved.free();
  setSetting(stamped);
  await persist(account, stamped);
  return stamped;
}

/**
 * A Visibility envelope from another device of this account. Kept only when
 * strictly newer than the held setting. Returns whether it was kept.
 */
export async function receiveVisibility(plaintext: Uint8Array): Promise<boolean> {
  const account = boundAccount;
  const current = setting();
  if (account === null) return false;
  const wasm = await loadWasm();
  let incoming: VisibilitySetting;
  try {
    incoming = wasm.decodeVisibilityEnvelope(plaintext);
  } catch {
    return false;
  }
  if (current !== null && !wasm.visibilityIsNewer(incoming.updatedAtMs, current.updatedAtMs)) {
    incoming.free();
    return false;
  }
  setSetting(incoming);
  await persist(account, incoming);
  return true;
}
