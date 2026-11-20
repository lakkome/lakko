// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Keeps this device's channel list and the account's channel-subscription
 * registers together (2026-11-13; ADR-0050 section 2.9 amendment, UX
 * `docs/ux/second-device.md` section 4a). Renders nothing; mounted once in
 * `App.tsx` beside the channel hydration, inside the membership-end provider
 * (a leave the account wrote on another device drops the channel here as an
 * unsubscribe does).
 *
 * Runs whenever the fold or the channel state changes, once the channel log
 * is hydrated and the register port is installed for the same account. The
 * decisions are `planChannelSubscriptions` (pure); this file performs them.
 * The first run after a launch is also how channels subscribed before the
 * registers existed reach them (contract rule 2).
 */
import { createEffect, onCleanup, type Component } from "solid-js";

import { useAuth } from "../auth/AuthContext";
import { useMembershipEnd } from "../chat/MembershipEndContext";
import { channelsActions, channelsState } from "../state/channels-state";
import { loadWasm } from "../wasm";
import { catchUpChannel, channelHandleOf } from "./channel-access";
import {
  channelIntents,
  channelSubscriptionPort,
  channelSubscriptionVersion,
  clearChannelIntent,
  type ChannelSubscriptionPort,
  type ChannelSubscriptionView,
} from "./channel-subscription-register";
import { planChannelSubscriptions, type LocalChannelState } from "./channel-subscription-plan";
import { subscriberKeysOfSecret } from "./composer";
import { b64ToBytes, bytesToB64 } from "../profile/avatar-model";
import { leaveChannelOnServer } from "./unsubscribe";
import { channelsLeftAloneByRegister } from "./orphaned-by-reset";

/** Gather what this device holds, for the planner. */
function localChannels(port: ChannelSubscriptionPort): LocalChannelState[] {
  const ids = new Set<string>();
  for (const c of channelsState.channels()) ids.add(c.id);
  for (const id of channelsState.subscribedSeedsByChannel().keys()) ids.add(id);
  for (const r of port.all()) ids.add(r.channelIdHex);
  const listed = new Map(channelsState.channels().map((c) => [c.id, c]));
  const out: LocalChannelState[] = [];
  for (const idHex of ids) {
    const handle = listed.get(idHex);
    const access = channelsState.accessByChannel().get(idHex);
    const seeds = channelsState.subscribedSeedsByChannel().get(idHex);
    out.push({
      idHex,
      listed: handle !== undefined,
      publishing: channelsState.keysByChannel().has(idHex),
      coPublisherGroupIdHex: channelsActions.coPublisherGroupIdOf(idHex) ?? null,
      coPublisherOwnerKey:
        access?.ownerAccountKeyB64 === undefined ? null : b64ToBytes(access.ownerAccountKeyB64),
      subscriberSecret: channelsState.subscriberKeysByChannel().get(idHex)?.secret ?? null,
      readKey: access?.readKeyB64 === null || access?.readKeyB64 === undefined ? null : b64ToBytes(access.readKeyB64),
      holdsSeeds: seeds !== undefined && seeds.size > 0,
      name: channelsState.metaByChannel().get(idHex)?.name ?? handle?.localName ?? "",
      handle: handle === undefined ? "" : (channelHandleOf(idHex) ?? ""),
    });
  }
  return out;
}

async function installKey(idHex: string, secret: Uint8Array): Promise<boolean> {
  const keys = await subscriberKeysOfSecret(secret);
  if (keys === null) return false;
  channelsActions.setSubscriberKeys(idHex, keys);
  return true;
}

async function addFromRegister(idHex: string, reg: ChannelSubscriptionView): Promise<void> {
  if (!(await installKey(idHex, reg.subscriberSecret))) return;
  if (reg.readKey.length === 32) {
    const readKeyB64 = bytesToB64(reg.readKey);
    channelsActions.updateAccess(idHex, (r) => ({ ...r, readKeyB64 }));
  }
  if (reg.role === "co-publisher" && reg.publisherGroupIdHex.length === 64) {
    channelsActions.setCoPublisherGroupId(idHex, reg.publisherGroupIdHex);
  }
  channelsActions.addChannel({
    id: idHex,
    localName: reg.name,
    localDescription: "",
    addedAtMs: Date.now(),
    ...(reg.handle.length > 0 ? { localHandle: reg.handle } : {}),
  });
  // The existing reading path from here: envelopes for the shared key, the
  // public seeds under the read key. The notifications start with the shared
  // rule's first read after subscribing.
  await catchUpChannel(idHex).catch(() => undefined);
}

export const ChannelSubscriptionSync: Component = () => {
  const auth = useAuth();
  const membershipEnd = useMembershipEnd();
  let running = false;
  let again = false;
  let timer: ReturnType<typeof setTimeout> | null = null;

  const account = (): string | null => {
    const s = auth.state();
    return s.kind === "session" ? s.account.accountId : null;
  };

  async function runOnce(): Promise<void> {
    const port = channelSubscriptionPort();
    const acct = account();
    if (port === null || acct === null || port.accountIdB64 !== acct || !channelsState.hydrated()) return;
    const wasm = await loadWasm();
    // A channel orphaned by a key reset (UX 16.6) is the person's to remove
    // from its own screen, and one removed that way stays removed: nothing
    // here adds it back or writes its register.
    const alone = channelsLeftAloneByRegister(acct, channelsState.orphanedByKeyReset());
    const held = new Map(port.all().filter((r) => !alone.has(r.channelIdHex)).map((r) => [r.channelIdHex, r]));
    const ending = new Set<string>();
    for (const id of held.keys()) if (membershipEnd.isChannelEnding(id)) ending.add(id);
    const steps = planChannelSubscriptions({
      local: localChannels(port).filter((c) => !alone.has(c.idHex)),
      held,
      intents: channelIntents(),
      ending,
      freshSecret: () => {
        const kp = wasm.ChannelSubscriberKeypair.generate();
        try {
          return kp.secret();
        } finally {
          kp.free();
        }
      },
    });
    for (const step of steps) {
      // Another account signed in meanwhile: nothing more is written.
      if (channelSubscriptionPort() !== port || account() !== acct) return;
      try {
        switch (step.kind) {
          case "write":
            await port.set(step.idHex, step.value);
            break;
          case "add":
            await addFromRegister(step.idHex, step.register);
            break;
          case "adopt-key": {
            if (!(await installKey(step.idHex, step.secret))) break;
            if (step.oldSecret !== null) {
              const old = await subscriberKeysOfSecret(step.oldSecret);
              // Best effort: a row the service keeps goes with its 90-day
              // subscriber prune.
              if (old !== null) await leaveChannelOnServer(step.idHex, old);
            }
            await catchUpChannel(step.idHex).catch(() => undefined);
            break;
          }
          case "read-key": {
            const readKeyB64 = bytesToB64(step.readKey);
            channelsActions.updateAccess(step.idHex, (r) => ({ ...r, readKeyB64 }));
            await catchUpChannel(step.idHex).catch(() => undefined);
            break;
          }
          case "co-publisher-mark":
            channelsActions.setCoPublisherGroupId(step.idHex, step.groupIdHex);
            break;
          case "drop":
            membershipEnd.channelLeftElsewhere(step.idHex);
            break;
          case "forget-seeds":
            channelsActions.clearSubscriberKeys(step.idHex);
            break;
          case "intent-done":
            clearChannelIntent(step.idHex);
            break;
        }
      } catch (err) {
        // A register out of bounds, a service that did not answer: asked
        // again at the next change.
        // eslint-disable-next-line no-console
        console.warn("[channels] a subscription step did not finish", step.kind, err instanceof Error ? err.name : "");
      }
    }
  }

  function schedule(): void {
    if (timer !== null) return;
    timer = setTimeout(() => {
      timer = null;
      if (running) {
        again = true;
        return;
      }
      running = true;
      void runOnce().finally(() => {
        running = false;
        if (again) {
          again = false;
          schedule();
        }
      });
    }, 0);
  }

  createEffect(() => {
    // Everything the plan reads.
    channelSubscriptionVersion();
    account();
    channelsState.hydrated();
    channelsState.channels();
    channelsState.keysByChannel();
    channelsState.subscriberKeysByChannel();
    channelsState.subscribedSeedsByChannel();
    channelsState.accessByChannel();
    channelsState.metaByChannel();
    schedule();
  });

  onCleanup(() => {
    if (timer !== null) clearTimeout(timer);
  });

  return null;
};
