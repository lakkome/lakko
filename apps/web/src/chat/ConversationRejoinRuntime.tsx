// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0091 phase 2 - the whole-session part of bringing a device back into a
 * conversation it fell behind in, mounted once beside the other session
 * runtimes in `App.tsx`. Renders nothing.
 *
 * - **Asking to come back** (section 3, `reentry-request.ts`): for every
 *   conversation this device is suspected or confirmed behind in, when the
 *   core says a request is due - right after a poll of that conversation,
 *   when the behind state changes, and on a one-minute tick.
 * - **Bringing another device back** (section 4, `reentry-admit.ts`): every
 *   request that arrives by one of the three routes is judged and, when
 *   admitted, the pair is proposed and the sweep asked to run.
 * - **The knock key** (section 3, route 3, `knock-key.ts`): the owner sends
 *   it in-group after a poll, when it changed or the group grew; whichever
 *   owner or admin brought a device back sends it once the replacement
 *   landed.
 * - **The gap's messages** (section 7, `reentry-history.ts`): the device that
 *   brought another back sends them once the replacement landed; the device
 *   that came back fetches them.
 */
import { createEffect, onCleanup, untrack, type JSX } from "solid-js";

import { useAuth } from "../auth/AuthContext";
import { base64Encode } from "../auth/account-id";
import { getDeviceKey } from "../auth/key-store";
import { ContactsClient, knockKeyFor } from "../contacts/client";
import { loadAccepted } from "../contacts/local-store";
import { DiscoveryClient } from "../discovery/client";
import { DsClient } from "../ds/client";
import { durableSnapshotHook } from "./persist-hooks";
import { pollForHistoryBundle } from "../devices/welcome-courier";
import { applyGapHistoryBundle, buildGapHistoryBundle } from "../devices/history-bundle";
import { useProfiles } from "../profile/ProfileContext";
import { redistributeSecretToGroup } from "../push/group-routing";
import { useGroupNames } from "./GroupNameContext";
import { usePins } from "./PinContext";
import { useSharedChannels } from "../discover/SharedChannelsContext";
import { sendServiceEnvelopeNow } from "../reach/service-envelope-outbox";
import { loadWasm, type Wasm } from "../wasm";
import type { Group as WasmGroup } from "../../wasm/lakko_ffi_wasm";
import { useBackgroundReceive } from "./BackgroundReceiveContext";
import { ChatClient, installPollObserver, rosterOf } from "./client";
import { useChats } from "./ChatContext";
import { conversationBehind, toWasmBehindState, type BehindVerdictWasm } from "./conversation-behind";
import { conversationKindOf } from "./conversation-kind";
import { readmissionCandidates } from "./device-readmission";
import { ownsGroup } from "./group-owner";
import { sendKnockKeyIfDue } from "./knock-key";
import { markOwnPublished } from "./own-published";
import { ownPersonKeyB64 } from "./person-key";
import { handleReentryRequest, installReentryIntake, type ReentryArrival } from "./reentry-admit";
import { afterGroupPollForGapHistory, emitGapHistoryFiled, noteGapHistoryOwed } from "./reentry-history";
import { installReentryJoinPorts } from "./reentry-join";
import { requestReentryIfDue, type ReentryRequestPorts } from "./reentry-request";
import { clearKnockKeySentMark, heldKnockKey, narrowGapLine, reentryRecordFor, type ReentryRouteName } from "./reentry-store";

/** How often behind conversations are looked at between polls (chosen, not
 *  measured: the request interval is 30 minutes). */
export const REJOIN_TICK_MS = 60_000;

/** How long the device that came back waits for the gap's messages (chosen,
 *  not measured: the sender posts them after its next poll of the group). */
export const GAP_HISTORY_WAIT_MS = 15 * 60_000;

/** The session account (standard base64), or `null`. */
function sessionAccount(state: ReturnType<ReturnType<typeof useAuth>["state"]>): string | null {
  return state.kind === "session" ? state.account.accountId : null;
}

function bytesToHex(bytes: Uint8Array): string {
  let out = "";
  for (const b of bytes) out += b.toString(16).padStart(2, "0");
  return out;
}

function hexToBytes(hex: string): Uint8Array | null {
  if (hex.length % 2 !== 0 || !/^[0-9a-f]*$/i.test(hex)) return null;
  const out = new Uint8Array(hex.length / 2);
  for (let i = 0; i < out.length; i++) out[i] = parseInt(hex.slice(i * 2, i * 2 + 2), 16);
  return out;
}

/** The core's route enum to its name. */
function routeName(wasm: Wasm, route: number): ReentryRouteName {
  if (route === wasm.ReentryRoute.OwnDevice) return "own-device";
  if (route === wasm.ReentryRoute.Mailbox) return "mailbox";
  return "knock";
}

export function ConversationRejoinRuntime(): JSX.Element {
  const auth = useAuth();
  const chats = useChats();
  const profiles = useProfiles();
  const groupNames = useGroupNames();
  const pins = usePins();
  const sharedChannels = useSharedChannels();
  const receive = useBackgroundReceive();
  const ds = new DsClient();
  const discovery = new DiscoveryClient();
  const persistHook = durableSnapshotHook(chats, () => sessionAccount(auth.state()));
  const contacts = new ContactsClient(discovery, undefined, {
    identityProvider: () => chats.identity(),
    accountIdentityProvider: () => chats.accountIdentity(),
    onPersist: persistHook,
  });
  const chat = new ChatClient(undefined, {
    identityProvider: () => chats.identity(),
    onPersist: persistHook,
  });

  async function persistDurable(): Promise<void> {
    const account = sessionAccount(auth.state());
    if (account === null) throw new Error("no session");
    await chats.persistSnapshotDurable(account);
  }

  function ownRoleOf(wasm: Wasm, group: WasmGroup): "owner" | "admin" | "member" | null {
    const identity = chats.identity();
    if (identity === null) return null;
    try {
      const role = group.ownRole(identity);
      if (role === undefined) return null;
      if (role === wasm.Role.Owner) return "owner";
      if (role === wasm.Role.Admin) return "admin";
      return "member";
    } catch {
      return null;
    }
  }

  // ---------- the request (section 3) ------------------------------------

  function requestPorts(wasm: Wasm): ReentryRequestPorts {
    return {
      now: () => Date.now(),
      identity: () => chats.identity(),
      ownPersonKeyB64: () => ownPersonKeyB64(chats.identity()),
      roster: (group) =>
        rosterOf(group).map((m) => ({
          leafKeyB64: base64Encode(m.signaturePublicKey),
          personKeyB64: base64Encode(m.accountKey ?? m.signaturePublicKey),
        })),
      kindOf: (group) => conversationKindOf(group),
      due: (state, lastSentMs, nowMs) => {
        const s = toWasmBehindState(wasm as unknown as BehindVerdictWasm, state);
        try {
          return wasm.reentryRequestDue(s, lastSentMs === undefined ? undefined : BigInt(lastSentMs), BigInt(nowMs));
        } finally {
          s.free();
        }
      },
      route: (own, kind) =>
        routeName(wasm, Number(wasm.reentryRoute(own, kind === "chat" ? wasm.GroupKind.OneToOne : wasm.GroupKind.Group))),
      knockKey: async (group) => {
        const held = heldKnockKey(base64Encode(group.groupId));
        if (held !== undefined) return held;
        const role = ownRoleOf(wasm, group);
        const account = chats.accountIdentity();
        if ((role !== "owner" && role !== "admin") || account === null) return null;
        const knock = await knockKeyFor(wasm, account, group, chats.conversationAccountIdentity());
        return knock === null ? null : { version: knock.version, publicKeyB64: base64Encode(knock.publicKey) };
      },
      persistDurable,
      composeEnvelope: (groupId, epochLeft, gapFromMs, kp) =>
        wasm.composeConversationReentryEnvelope(groupId, epochLeft, gapFromMs, kp),
      sealKnockRow: (pk, version, envelope) => wasm.sealReentryKnockRow(pk, version, envelope),
      sendOwnDevice: (envelope) => sendServiceEnvelopeNow(envelope),
      sendMailbox: (other, envelope, groupId) => contacts.sendConversationReentry(other, envelope, groupId),
      sendKnock: async (groupId, row) => {
        await ds.knock(groupId, [row]);
      },
    };
  }

  /** Whether a request could be due now (the probe costs a token: only then). */
  function reentryRequestMayBeDue(groupIdB64: string, state: ReturnType<typeof conversationBehind.state>): boolean {
    if (!state.stopsActing) return false;
    const record = reentryRecordFor(groupIdB64);
    return record === undefined || record.lastSentMs <= 0 || Date.now() - record.lastSentMs >= 30 * 60_000;
  }

  /** One attempt per group at a time. */
  const requesting = new Set<string>();

  async function requestFor(groupIdB64: string): Promise<void> {
    if (requesting.has(groupIdB64)) return;
    const state = conversationBehind.state(groupIdB64);
    if (!state.stopsActing) return;
    const entry = chats.get(groupIdB64);
    if (entry === undefined) return;
    requesting.add(groupIdB64);
    try {
      let epochLeft = conversationBehind.latestEpoch(groupIdB64);
      if (epochLeft === undefined) {
        // Suspected only through refused reads: no read named the journal's
        // epoch. An empty knock is the probe (its answer carries the latest
        // registered epoch; one `ds-knock` token), as on iOS and Android.
        if (!reentryRequestMayBeDue(groupIdB64, state)) return;
        const probe = await ds.knock(entry.group.groupId, []).catch(() => null);
        if (probe === null || probe.epoch === null) return;
        epochLeft = BigInt(probe.epoch);
      }
      const wasm = await loadWasm();
      await requestReentryIfDue(
        entry.group,
        {
          state,
          epochLeft,
          gapFromMs: conversationBehind.gap(groupIdB64).gapFromMs ?? state.sinceMs ?? Date.now(),
        },
        requestPorts(wasm),
      );
    } finally {
      requesting.delete(groupIdB64);
    }
  }

  function requestAllDue(): void {
    if (sessionAccount(auth.state()) === null) return;
    for (const g of conversationBehind.behindGroups()) void requestFor(g);
  }

  createEffect(() => {
    conversationBehind.version();
    untrack(() => requestAllDue());
  });
  const tick = setInterval(requestAllDue, REJOIN_TICK_MS);
  onCleanup(() => clearInterval(tick));

  // ---------- bringing another device back (section 4) -------------------

  async function directoryListsDevice(accountKey: Uint8Array, deviceKey: Uint8Array): Promise<boolean> {
    const keyB64 = base64Encode(accountKey);
    const [candidate] = readmissionCandidates({
      rosterPersonKeysB64: [keyB64],
      ownPersonKeyB64: "",
      contacts: loadAccepted(),
      profileAccountIdHexOf: (k) => profiles.accountIdHexOf(k),
    });
    const accountId = candidate === undefined ? null : hexToBytes(candidate.accountIdHex);
    // An account this device cannot name is not checked: refused.
    if (accountId === null) return false;
    const list = await discovery.fetchKeyPackages(accountId);
    const hex = bytesToHex(deviceKey);
    return (list ?? []).some((d) => bytesToHex(d.deviceId) === hex);
  }

  let admitChain: Promise<unknown> = Promise.resolve();

  function admit(arrival: ReentryArrival): void {
    admitChain = admitChain
      .then(async () => {
        const wasm = await loadWasm();
        const secrets = auth.identitySecrets();
        if (secrets === null) return;
        await handleReentryRequest(arrival, {
          now: () => Date.now(),
          identity: () => chats.identity(),
          ownPersonKeyB64: () => ownPersonKeyB64(chats.identity()),
          group: (g) => chats.get(g)?.group,
          ownRole: (group) => ownRoleOf(wasm, group),
          kindOf: (group) => conversationKindOf(group),
          directoryListsDevice,
          admissionVerdict: (i) =>
            Number(
              wasm.reentryAdmissionVerdict(
                i.stillTheAccounts,
                i.lastWriteMs === undefined ? undefined : BigInt(i.lastWriteMs),
                i.handledAtThisEpoch,
                i.lastAcceptedMs === undefined ? undefined : BigInt(i.lastAcceptedMs),
                BigInt(i.nowMs),
              ),
            ),
          propose: async (group, keyPackage) => {
            const groupIdB64 = base64Encode(group.groupId);
            await contacts.proposeReentryReplacement(secrets, group, keyPackage, (id) => markOwnPublished(groupIdB64, id));
            // The sweep commits the pair: the open chat's own poll, or one now.
            if (receive.coordinator.isForeground(groupIdB64)) receive.coordinator.requestPoll(groupIdB64);
            else void receive.syncGroupNow(groupIdB64).catch(() => undefined);
          },
          noteGapOwed: (o) =>
            noteGapHistoryOwed({
              groupIdB64: o.groupIdB64,
              leafKey: o.leafKey,
              epochAfter: o.epochAfter,
              gapFromMs: o.gapFromMs,
              nowMs: Date.now(),
            }),
        });
      })
      .catch(() => undefined);
  }

  onCleanup(() => installReentryIntake(null));
  installReentryIntake(admit);

  // ---------- after every applied poll ------------------------------------

  async function sendGapBundle(group: WasmGroup, secret: Uint8Array, fromMs: number): Promise<Uint8Array | null> {
    const account = sessionAccount(auth.state());
    if (account === null) return null;
    const deviceKey = await getDeviceKey(account);
    if (deviceKey === null) return null;
    return buildGapHistoryBundle({ accountIdB64: account, deviceKey, secret, slotHex: bytesToHex(group.groupId), fromMs });
  }

  /**
   * The replacement landed: re-send what a newcomer receives on growth (name,
   * photo, pins, profile, shared channels, push routing secret, knock key).
   * A replacement changes no member set, so no member sees growth, and the
   * returning device holds nothing of it at the new epoch. Best-effort, each
   * through its own throttled path.
   */
  function resendGrowthState(group: WasmGroup): void {
    const groupIdB64 = base64Encode(group.groupId);
    profiles.groupGrew(groupIdB64);
    groupNames.groupGrew(groupIdB64);
    pins.groupGrew(groupIdB64);
    sharedChannels.groupGrew(groupIdB64);
    clearKnockKeySentMark(groupIdB64);
    // The knock key: the owner and the admins hold the knock seeds, so
    // whichever of them brought the device back sends it now (a member
    // keeps it only from the owner or an admin).
    void (async () => {
      const wasm = await loadWasm();
      const role = ownRoleOf(wasm, group);
      const accountIdentity = chats.accountIdentity();
      if ((role !== "owner" && role !== "admin") || accountIdentity === null || conversationKindOf(group) !== "group") return;
      const knock = await knockKeyFor(wasm, accountIdentity, group, chats.conversationAccountIdentity());
      if (knock === null) return;
      await chat.broadcastControl(auth.identitySecrets(), group, wasm.composeKnockKeyEnvelope(knock.version, knock.publicKey));
    })().catch((err: unknown) => {
      // eslint-disable-next-line no-console
      console.warn(`chat re-entry: knock key not re-sent: ${err instanceof Error ? err.name : "error"}`);
    });
    const account = sessionAccount(auth.state());
    if (account !== null) {
      void redistributeSecretToGroup(account, group, async (envelope) => {
        await chat.broadcastControl(auth.identitySecrets(), group, envelope);
      }).catch(() => undefined);
    }
  }

  async function onPolled(group: WasmGroup): Promise<void> {
    let groupIdB64: string;
    try {
      groupIdB64 = base64Encode(group.groupId);
    } catch {
      return;
    }
    if (conversationBehind.stopsActing(groupIdB64)) {
      await requestFor(groupIdB64);
      return;
    }
    await afterGroupPollForGapHistory(group, {
      now: () => Date.now(),
      identity: () => chats.identity(),
      landed: (g) => resendGrowthState(g),
      build: sendGapBundle,
      publish: async (rendezvous, sealed) => {
        await ds.publishRendezvous(rendezvous, sealed);
      },
    });
    // The owner sends the knock key; nobody else does.
    const account = chats.accountIdentity();
    if (
      account === null ||
      conversationKindOf(group) !== "group" ||
      !ownsGroup(group, [account.signaturePublicKey, chats.conversationAccountIdentity()?.signaturePublicKey])
    ) {
      return;
    }
    const wasm = await loadWasm();
    await sendKnockKeyIfDue(group, {
      ownerKnockKey: async (g) => {
        const knock = await knockKeyFor(wasm, account, g, chats.conversationAccountIdentity());
        return knock === null ? null : { version: knock.version, publicKey: knock.publicKey };
      },
      memberCount: (g) => rosterOf(g).length,
      compose: (version, pk) => wasm.composeKnockKeyEnvelope(version, pk),
      broadcast: async (g, envelope) => {
        await chat.broadcastControl(auth.identitySecrets(), g, envelope);
      },
    });
  }

  installPollObserver((group) => {
    void onPolled(group).catch(() => undefined);
  });
  onCleanup(() => installPollObserver(null));

  // ---------- coming back (section 5) and the gap (section 7) -------------

  installReentryJoinPorts({
    now: () => Date.now(),
    identity: () => chats.identity(),
    persistDurable,
    fetchGapHistory: ({ groupIdB64, channel, gapToMs }) => {
      void (async () => {
        const account = sessionAccount(auth.state());
        if (account === null) return;
        const outcome = await pollForHistoryBundle(channel.rendezvous, 0, { timeoutMs: GAP_HISTORY_WAIT_MS });
        if (outcome.kind !== "received") return;
        const deviceKey = await getDeviceKey(account);
        if (deviceKey === null) return;
        const filed = await applyGapHistoryBundle({
          accountIdB64: account,
          deviceKey,
          secret: channel.secret,
          sealedBytes: outcome.sealedBytes,
        });
        narrowGapLine(groupIdB64, gapToMs, { complete: filed.complete, earliestMs: filed.earliestMs });
        // An open chat shows them now, without reloading the log.
        emitGapHistoryFiled(groupIdB64, filed.rows);
        // eslint-disable-next-line no-console
        console.info(`chat re-entry: missed messages received: rows=${filed.applied}`);
      })().catch((err: unknown) => {
        // eslint-disable-next-line no-console
        console.warn(`chat re-entry: missed messages not received: ${err instanceof Error ? err.name : "error"}`);
      });
    },
  });
  onCleanup(() => installReentryJoinPorts(null));

  return null;
}
