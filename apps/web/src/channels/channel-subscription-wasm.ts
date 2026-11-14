// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The channel-subscription register over the wasm `SyncedState`: what the
 * DeviceGroup provider calls, kept apart so a test can drive two real folds
 * through exactly the code the provider runs. Secrets are copied, never
 * logged.
 */
import type { Wasm } from "../wasm";
import type { ChannelSubscriptionRegister, ChannelSubscriptionRole } from "../../wasm/lakko_ffi_wasm";
import type {
  ChannelSubscriptionRoleName,
  ChannelSubscriptionValue,
  ChannelSubscriptionView,
} from "./channel-subscription-register";

type SyncedState = InstanceType<Wasm["SyncedState"]>;
type Register = ChannelSubscriptionRegister;
type RoleEnum = Wasm["ChannelSubscriptionRole"];

export function hexOfBytes(bytes: Uint8Array): string {
  let out = "";
  for (const b of bytes) out += b.toString(16).padStart(2, "0");
  return out;
}

/** Lower-case or upper-case hex to bytes; `null` when it is not hex of an
 *  even length. */
export function bytesOfHex(hex: string): Uint8Array | null {
  if (hex.length % 2 !== 0 || !/^[0-9a-fA-F]*$/.test(hex)) return null;
  const out = new Uint8Array(hex.length / 2);
  for (let i = 0; i < out.length; i += 1) out[i] = Number.parseInt(hex.slice(i * 2, i * 2 + 2), 16);
  return out;
}

function roleName(role: ChannelSubscriptionRole, roles: RoleEnum): ChannelSubscriptionRoleName {
  if (role === roles.Owner) return "owner";
  if (role === roles.CoPublisher) return "co-publisher";
  return "subscriber";
}

function roleValue(role: ChannelSubscriptionRoleName, roles: RoleEnum): ChannelSubscriptionRole {
  if (role === "owner") return roles.Owner;
  if (role === "co-publisher") return roles.CoPublisher;
  return roles.Subscriber;
}

/** Read one wasm register into a plain view and free it. */
export function viewOfRegister(reg: Register, roles: RoleEnum): ChannelSubscriptionView {
  try {
    return {
      channelIdHex: hexOfBytes(reg.channelId),
      hlc: reg.hlc,
      subscribed: reg.subscribed,
      role: roleName(reg.role, roles),
      subscriberSecret: reg.subscriberSecret.slice(),
      readKey: reg.readKey.slice(),
      publisherGroupIdHex: hexOfBytes(reg.publisherGroupId),
      ownerAccountKey: reg.ownerAccountKey.slice(),
      name: reg.name,
      handle: reg.handle,
    };
  } finally {
    reg.free();
  }
}

/** Every register the fold holds, leaves included. */
export function allChannelSubscriptionViews(synced: SyncedState, wasm: Wasm): ChannelSubscriptionView[] {
  return synced.allChannelSubscriptions().map((r) => viewOfRegister(r, wasm.ChannelSubscriptionRole));
}

/** The register of one channel, or `undefined` (also for a malformed id). */
export function channelSubscriptionView(
  synced: SyncedState,
  wasm: Wasm,
  channelIdHex: string,
): ChannelSubscriptionView | undefined {
  const id = bytesOfHex(channelIdHex);
  if (id === null || id.length !== 16) return undefined;
  const reg = synced.channelSubscription(id);
  return reg === undefined ? undefined : viewOfRegister(reg, wasm.ChannelSubscriptionRole);
}

/** Write a live register at `hlc` (the fold stamps it past the one held).
 *  Returns the envelope to send, or `null` when the identical value is held.
 *  Throws `bounds:` for a field out of bounds. */
export function writeChannelSubscription(
  synced: SyncedState,
  wasm: Wasm,
  hlc: bigint,
  channelIdHex: string,
  value: ChannelSubscriptionValue,
): Uint8Array | null {
  const id = bytesOfHex(channelIdHex);
  if (id === null || id.length !== 16) throw new Error("bounds: channel id");
  const groupId =
    value.role === "co-publisher" ? (bytesOfHex(value.publisherGroupIdHex) ?? new Uint8Array(0)) : new Uint8Array(0);
  const written = synced.setChannelSubscription(
    hlc,
    id,
    roleValue(value.role, wasm.ChannelSubscriptionRole),
    value.subscriberSecret,
    value.readKey.length === 32 ? value.readKey : new Uint8Array(0),
    groupId,
    // ADR-0090: the owner key only for a co-publisher (refused otherwise).
    value.role === "co-publisher" ? value.ownerAccountKey : new Uint8Array(0),
    value.name,
    value.handle,
  );
  if (!written) return null;
  return synced.channelSubscriptionEnvelopeFor(id) ?? null;
}

/** Write the leave at `hlc`. Returns the envelope to send, or `null` when
 *  nothing live was held. */
export function writeChannelLeave(synced: SyncedState, hlc: bigint, channelIdHex: string): Uint8Array | null {
  const id = bytesOfHex(channelIdHex);
  if (id === null || id.length !== 16) return null;
  if (!synced.leaveChannelSubscription(hlc, id)) return null;
  return synced.channelSubscriptionEnvelopeFor(id) ?? null;
}
