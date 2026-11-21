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
 *   brought another back sends them once the replacement landed (from every
 *   applied poll, and from the one-minute tick, which also reads a hidden
 *   tab's owed group once a minute: a parked background sweep may not poll
 *   it); the device that came back keeps looking for them, durably, until
 *   they arrive or the DS retention passed (live walk 2026-11-19).
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
import { applyGapHistoryBundle, buildGapHistoryBundle } from "../devices/history-bundle";
import { useProfiles } from "../profile/ProfileContext";
import { redistributeSecretToGroup } from "../push/group-routing";
import { useGroupNames } from "./GroupNameContext";
import { usePins } from "./PinContext";
import { useSharedChannels } from "../discover/SharedChannelsContext";
import { hasServiceEnvelopeSender, sendServiceEnvelopeNow } from "../reach/service-envelope-outbox";
import { loadWasm, type Wasm } from "../wasm";
import type { Group as WasmGroup, Identity as WasmIdentity } from "../../wasm/lakko_ffi_wasm";
import { useBackgroundReceive } from "./BackgroundReceiveContext";
import { ChatClient, installPollObserver, rosterOf } from "./client";
import { useChats } from "./ChatContext";
import { conversationBehind, toWasmBehindState, type BehindVerdictWasm } from "./conversation-behind";
import { conversationKindOf } from "./conversation-kind";
import { lastLeafWriteAnyEpoch, resendsPushRoutingSecret } from "./reentry-admit";
import { readmissionCandidates } from "./device-readmission";
import { ownsGroup } from "./group-owner";
import { sendKnockKeyIfDue } from "./knock-key";
import { markOwnPublished } from "./own-published";
import { ownPersonKeyB64 } from "./person-key";
import { resolveMemberIdentity, type RosterMember } from "./author-identity";
import { handleReentryRequest, installReentryIntake, type ReentryArrival } from "./reentry-admit";
import {
  afterGroupPollForGapHistory,
  emitGapHistoryFiled,
  installGapLookRequester,
  lookForGapHistory,
  noteGapHistoryOwed,
  owedGapHistoryGroups,
  recordGapLook,
  sealGapSecret,
  type GapLookPorts,
  type GapSendPorts,
} from "./reentry-history";
import { installReentryJoinPorts } from "./reentry-join";
import { requestReentryIfDue, type ReentryRequestPorts } from "./reentry-request";
import {
  clearKnockKeySentMark,
  endReentryEpisode,
  gapLookGroups,
  heldKnockKey,
  leafFirstSeenMs,
  narrowGapLine,
  type ReentryRouteName,
} from "./reentry-store";

/** How often behind conversations are looked at between polls (chosen, not
 *  measured: the request interval is 30 minutes). */
export const REJOIN_TICK_MS = 60_000;

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
      due: (state, lastSentMs, lastRouteWasOwnDevice, nowMs) => {
        const s = toWasmBehindState(wasm as unknown as BehindVerdictWasm, state);
        try {
          return wasm.reentryRequestDue(
            s,
            lastSentMs === undefined ? undefined : BigInt(lastSentMs),
            lastRouteWasOwnDevice,
            BigInt(nowMs),
          );
        } finally {
          s.free();
        }
      },
      route: (own, kind, ownDeviceUnanswered) =>
        routeName(
          wasm,
          Number(
            wasm.reentryRoute(own, kind === "chat" ? wasm.GroupKind.OneToOne : wasm.GroupKind.Group, ownDeviceUnanswered),
          ),
        ),
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
      generateGapKey: () => {
        const key = wasm.generateGapKey();
        try {
          return { secret: key.secret, publicKey: key.publicKey };
        } finally {
          key.free();
        }
      },
      sealGapSecret: async (secret) => {
        const account = sessionAccount(auth.state());
        if (account === null) throw new Error("no session");
        const deviceKey = await getDeviceKey(account);
        if (deviceKey === null) throw new Error("no device key");
        return sealGapSecret(secret, deviceKey);
      },
      composeEnvelope: (groupId, epochLeft, gapFromMs, gapKey, kp) =>
        wasm.composeConversationReentryEnvelope(groupId, epochLeft, gapFromMs, gapKey, kp),
      sealKnockRow: (pk, version, envelope) => wasm.sealReentryKnockRow(pk, version, envelope),
      sendOwnDevice: (envelope) => sendServiceEnvelopeNow(envelope),
      ownDeviceReachable: () => hasServiceEnvelopeSender(),
      sendMailbox: (other, envelope, groupId) => contacts.sendConversationReentry(other, envelope, groupId),
      sendKnock: async (groupId, row) => {
        await ds.knock(groupId, [row]);
      },
      // Live walk 3: every request names the epoch the DS answers now (an
      // empty knock, one `ds-knock` token), never a stored one.
      probeEpoch: async (groupId) => {
        const probe = await ds.knock(groupId, []).catch(() => null);
        return probe === null || probe.epoch === null ? null : BigInt(probe.epoch);
      },
    };
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
      // `requestReentryIfDue` asks the core's `reentryRequestDue` before the
      // epoch probe, so no token is spent on a request that is not due.
      const wasm = await loadWasm();
      await requestReentryIfDue(
        entry.group,
        {
          state,
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
  const tick = setInterval(() => {
    requestAllDue();
    sendOrFetchOwed();
    lookOpenChats();
  }, REJOIN_TICK_MS);
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
              gapKey: o.gapKey,
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

  async function sendGapBundle(
    group: WasmGroup,
    readmitter: WasmIdentity,
    gapKey: Uint8Array,
    fromMs: number,
  ): Promise<Uint8Array | null> {
    const account = sessionAccount(auth.state());
    if (account === null) return null;
    const deviceKey = await getDeviceKey(account);
    if (deviceKey === null) return null;
    return buildGapHistoryBundle({
      accountIdB64: account,
      deviceKey,
      readmitter,
      gapKey,
      slotHex: bytesToHex(group.groupId),
      fromMs,
      ownAuthorKeyB64: ownAuthorKeyIn(group),
    });
  }

  /**
   * Live walk 3 — this account's key in `group` as every sent row names it:
   * the person this device's own leaf resolves to (`send-text.ts`). `null`
   * when the roster cannot be read.
   */
  function ownAuthorKeyIn(group: WasmGroup | undefined): string | null {
    if (group === undefined) return null;
    try {
      return resolveMemberIdentity(group.members() as RosterMember[], group.ownLeafIndex) ?? null;
    } catch {
      return null;
    }
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
    // The push routing secret only from the owner or an admin: the receive
    // gate (ADR-0042) keeps a plain member's secret only when it is the
    // bytes already held, so a plain member's re-send is refused there (live
    // walk 2026-11-19). A device brought back by a plain member gets the
    // secret at the owner's next rotation or redistribution (G-58).
    const account = sessionAccount(auth.state());
    if (account !== null) {
      void (async () => {
        const wasm = await loadWasm();
        if (!resendsPushRoutingSecret(ownRoleOf(wasm, group))) return;
        await redistributeSecretToGroup(account, group, async (envelope) => {
          await chat.broadcastControl(auth.identitySecrets(), group, envelope);
        });
      })().catch(() => undefined);
    }
  }

  function gapSendPorts(wasm: Wasm): GapSendPorts {
    return {
      now: () => Date.now(),
      // The leaf that signs is this device's leaf in the conversation.
      identity: () => chats.identity(),
      landed: (g: WasmGroup) => resendGrowthState(g),
      leafFirstSeenMs: (g, leaf) => leafFirstSeenMs(g, leaf),
      leafLastWriteMs: (g, leaf) => lastLeafWriteAnyEpoch(g, leaf),
      gapStart: (claimed, firstSeen, lastWrite) => {
        const start = wasm.gapHistoryStart(
          BigInt(Math.max(0, Math.floor(claimed))),
          firstSeen === undefined ? undefined : BigInt(Math.max(0, Math.floor(firstSeen))),
          lastWrite === undefined ? undefined : BigInt(Math.max(0, Math.floor(lastWrite))),
        );
        return start === undefined ? null : Number(start);
      },
      build: sendGapBundle,
      rendezvous: (gapKey) => wasm.gapHistoryRendezvous(gapKey),
      publish: async (rendezvous: Uint8Array, sealed: Uint8Array) => {
        await ds.publishRendezvous(rendezvous, sealed);
      },
    };
  }

  /**
   * The owed sends, without waiting for a poll: one whose group already
   * stands at the replacement's epoch (this device's own sweep merged it) is
   * sent now; one below it gets a one-off read of its group when no open
   * chat polls it (a hidden tab whose background sweep is parked). At most
   * once per tick per group.
   */
  function sendOrFetchOwed(): void {
    if (sessionAccount(auth.state()) === null) return;
    for (const { groupIdB64, epochAfter } of owedGapHistoryGroups()) {
      const entry = chats.get(groupIdB64);
      if (entry === undefined) continue;
      let epoch: bigint;
      try {
        epoch = entry.group.epoch;
      } catch {
        continue;
      }
      if (epoch >= epochAfter) {
        void loadWasm()
          .then((wasm) => afterGroupPollForGapHistory(entry.group, gapSendPorts(wasm)))
          .catch(() => undefined);
      } else if (!receive.coordinator.isForeground(groupIdB64)) {
        void receive.syncGroupNow(groupIdB64).catch(() => undefined);
      }
    }
  }

  // ---------- the device that came back looks for the gap -----------------

  function gapLookPorts(wasm: Wasm): GapLookPorts {
    return {
      now: () => Date.now(),
      deviceKey: async () => {
        const account = sessionAccount(auth.state());
        return account === null ? null : getDeviceKey(account);
      },
      rendezvousOf: (secret) => wasm.gapHistoryRendezvous(wasm.gapPublicKey(secret)),
      readPage: async (rendezvous, after, limit) => {
        const result = await ds.fetchRendezvous(rendezvous, { after, limit, waitSecs: 0 });
        return {
          rows: result.messages.map((m) => ({ id: m.id, payload: m.payload })),
          hasMore: result.hasMore,
        };
      },
      apply: async (secret, sealed, groupIdB64, route) => {
        const account = sessionAccount(auth.state());
        if (account === null) return null;
        const deviceKey = await getDeviceKey(account);
        if (deviceKey === null) return null;
        const group = chats.get(groupIdB64)?.group;
        const identity = chats.identity();
        if (group === undefined || identity === null) return null;
        // Live walk 3: a bundled row names its author; it is this account's
        // own when the author is its key in the conversation.
        return applyGapHistoryBundle({
          accountIdB64: account,
          deviceKey,
          gapSecret: secret,
          groupId: group.groupId,
          sealedBytes: sealed,
          // Security review 2026-11-21: kept only from the own account, the
          // owner or an admin, or the other person of a one-to-one.
          signerAccepted: (signer) => group.gapHistorySignerAccepted(identity, signer),
          ownAuthorKeyB64: ownAuthorKeyIn(group),
          route,
        });
      },
      received: (groupIdB64, gapToMs, filed) => {
        narrowGapLine(groupIdB64, gapToMs, { complete: filed.complete, earliestMs: filed.earliestMs });
        // An open chat shows them now, without reloading the log.
        emitGapHistoryFiled(groupIdB64, filed.rows);
      },
    };
  }

  function lookFor(groupIdB64: string): void {
    if (sessionAccount(auth.state()) === null) return;
    void loadWasm()
      .then((wasm) => lookForGapHistory(groupIdB64, gapLookPorts(wasm)))
      .catch(() => undefined);
  }

  /** An open chat is looked for once a minute even between its polls. */
  function lookOpenChats(): void {
    for (const g of gapLookGroups()) {
      if (receive.coordinator.isForeground(g)) lookFor(g);
    }
  }

  // ADR-0091 live walk 3 — back in step: re-send this device's own group
  // state (profile, name, photo, reactions setting, handle, pins, shared
  // channels), which it held while behind and which nothing else would send
  // before the group next grows.
  onCleanup(
    conversationBehind.onBackInStep((groupIdB64) => {
      // Live walk 5: the behind episode ends; the next one asks its own
      // device first again.
      endReentryEpisode(groupIdB64);
      const entry = chats.get(groupIdB64);
      if (entry !== undefined) resendGrowthState(entry.group);
    }),
  );

  installGapLookRequester(lookFor);
  onCleanup(() => installGapLookRequester(null));

  async function onPolled(group: WasmGroup): Promise<void> {
    let groupIdB64: string;
    try {
      groupIdB64 = base64Encode(group.groupId);
    } catch {
      return;
    }
    // The roster read records the first sighting of each leaf (section 7).
    rosterOf(group);
    // A look for the gap's messages is a read: it runs whatever else does.
    lookFor(groupIdB64);
    if (conversationBehind.stopsActing(groupIdB64)) {
      await requestFor(groupIdB64);
      return;
    }
    await afterGroupPollForGapHistory(group, gapSendPorts(await loadWasm()));
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
      memberKeys: (g) => rosterOf(g).map((m) => base64Encode(m.signaturePublicKey)),
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
    fetchGapHistory: ({ groupIdB64, secrets, gapFromMs, gapToMs, route }) => {
      void (async () => {
        if (sessionAccount(auth.state()) === null) return;
        // Durable: the device keeps looking until the messages arrive or the
        // DS retention passed, across restarts (live walk 2026-11-19: the
        // re-admitter posted after a one-shot wait had closed). The secrets
        // are already sealed under the device key.
        const stored = recordGapLook({ groupIdB64, secrets, gapFromMs, gapToMs, route });
        if (!stored) {
          // eslint-disable-next-line no-console
          console.warn("chat re-entry: gap look not recorded");
          return;
        }
        await lookForGapHistory(groupIdB64, gapLookPorts(await loadWasm()));
      })().catch((err: unknown) => {
        // eslint-disable-next-line no-console
        console.warn(`chat re-entry: gap look failed: ${err instanceof Error ? err.name : "error"}`);
      });
    },
  });
  onCleanup(() => installReentryJoinPorts(null));

  return null;
}
