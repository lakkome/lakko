// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The app-level owner of the outbox (`docs/ux/connection-state.md` section
 * 7.3): reads the signed-in account's unsent messages back at the session
 * landing, and sends every conversation's waiting messages once the servers
 * answer and on every return to Online, whether or not the conversation is
 * open. An open chat view attaches to the same runner for its conversation
 * (`routes/Chat.tsx`), so there is one pass per conversation in the tab.
 *
 * Mounted inside `BackgroundReceiveProvider` (it uses the lineage and the
 * membership-end contexts) and above the router.
 */

import { createContext, createEffect, onCleanup, untrack, useContext, type JSX } from "solid-js";
import { base64Encode } from "../auth/account-id";
import { useAuth } from "../auth/AuthContext";
import { getDeviceKey } from "../auth/key-store";
import { contentRetryAllowed } from "../attach/actions";
import { receiveContent } from "../attach/receive";
import { groupIdHex } from "../devices/sync/state";
import { MAX_GIF_FULL_BYTES, fetchGifMedia } from "../media/gif-client";
import { connectionPhase, onServersAnswer } from "../net/connection-state";
import { sealedWakeForGroup } from "../push/group-routing";
import { encryptAndUploadBytes, type AttachmentRefBytes } from "./attachments";
import { appendToGroupSlot } from "./background-log";
import { ChatClient, HeldForSuccessor } from "./client";
import { useChats, type ActiveGroup } from "./ChatContext";
import { composeHeldEnvelope, heldSends, notifyHeldFlushed, persistHeldSends } from "./held-send-queue";
import { readIsActive } from "./membership-ended";
import { useMembershipEnd } from "./MembershipEndContext";
import { MESSAGE_LOG_VERSION, updateMessageLog, type PersistedMessage } from "./message-log-store";
import { persistedFromAttachmentRef } from "./persisted-attachment";
import { durableSnapshotHook, sessionAccountOf } from "./persist-hooks";
import { personKeysOf } from "./person-key";
import { persistedFromReplyRef } from "./reply-snapshot";
import { persistedFromForwardRef } from "./forward";
import { unsentOutbox, type UnsentEntry, type UnsentOutbox } from "./unsent-outbox";
import { sendUnsentHeadless, UnsentRunner, type HeadlessDeps } from "./unsent-runner";
import { runWhenServersAnswer, isNetworkSendFailure } from "./waiting-sends";
import { useLineage } from "./use-lineage";
import { adoptTrayUpload } from "./tray-store";
import { uploadTrayPart } from "./tray-upload";
import { ContactsClient } from "../contacts/client";
import { loadSupersededKeys } from "../contacts/local-store";
import { DiscoveryClient } from "../discovery/client";
import { conversationKindOf } from "./conversation-kind";
import { ownPersonKeyB64 } from "./person-key";
import {
  checkPeerIdentityIfDue,
  oneToOnePeerKey,
  oneToOneSendVerdict,
  oneToOneUnrepairableKey,
} from "./one-to-one-send-gate";

/** What a chat view uses of the outbox. */
export interface UnsentOutboxApi {
  readonly outbox: UnsentOutbox;
  readonly runner: UnsentRunner;
}

const UnsentOutboxContext = createContext<UnsentOutboxApi>();

/** The outbox and its runner; throws outside the provider. */
export function useUnsentOutbox(): UnsentOutboxApi {
  const api = useContext(UnsentOutboxContext);
  if (api === undefined) throw new Error("useUnsentOutbox outside UnsentOutboxProvider");
  return api;
}

/** The sent row as the message log stores it. Pure. */
export function sentRowOf(
  entry: UnsentEntry,
  attachments: readonly AttachmentRefBytes[],
  result: { messageId: number; createdAt: string },
  authorKeyB64: string | undefined,
): PersistedMessage {
  return {
    messageId: String(result.messageId),
    direction: "outgoing",
    text: entry.text,
    attachments: attachments.map((a) => persistedFromAttachmentRef(a)),
    createdAt: result.createdAt,
    ...(authorKeyB64 === undefined ? {} : { authorKeyB64 }),
    ...(entry.reply === undefined ? {} : { reply: persistedFromReplyRef(entry.reply) }),
    ...(entry.forward === undefined ? {} : { forward: persistedFromForwardRef(entry.forward) }),
    ...(entry.content === undefined ? {} : { content: entry.content }),
  };
}

export function UnsentOutboxProvider(props: { children: JSX.Element }): JSX.Element {
  const auth = useAuth();
  const chats = useChats();
  const lineage = useLineage();
  const membershipEnd = useMembershipEnd();

  const chat = new ChatClient(undefined, {
    identityProvider: () => chats.identity(),
    onPersist: durableSnapshotHook(chats, () => sessionAccountOf(auth.state())),
    wakeSealer: async (groupId, kind) => {
      const s = auth.state();
      if (s.kind !== "session") return null;
      return sealedWakeForGroup(s.account.accountId, base64Encode(groupId), kind);
    },
  });

  // The directory client the peer identity check runs through, as the open
  // view's (`routes/Chat.tsx`).
  const contacts = new ContactsClient(
    new DiscoveryClient(undefined, {
      sessionToken: () => auth.identitySecrets()?.sessionToken ?? null,
    }),
    undefined,
    {
      identityProvider: () => chats.identity(),
      conversationAccountIdentityProvider: () => chats.conversationAccountIdentity(),
      onPersist: durableSnapshotHook(chats, () => sessionAccountOf(auth.state())),
    },
  );

  const ownKeyB64 = (): string | undefined => {
    try {
      const id = chats.identity();
      return id === null ? undefined : base64Encode(id.signaturePublicKey);
    } catch {
      return undefined;
    }
  };

  const deps: HeadlessDeps<ActiveGroup> = {
    group: (g) => chats.get(g),
    ended: (entry) => readIsActive(entry.group) === false,
    contentSendable: (entry, g) => contentRetryAllowed(entry.content, personKeysOf(g.group)),
    preparedFile: (entry) => unsentOutbox.preparedFile(entry.groupIdB64, entry.tempId),
    loadGif: async (url) => {
      const token = auth.identitySecrets()?.sessionToken ?? null;
      if (token === null) throw new Error("no session for the GIF proxy");
      const r = await fetchGifMedia(url, token, MAX_GIF_FULL_BYTES);
      if (r.kind !== "ok") throw new Error(`GIF download failed: ${r.kind}`);
      return { bytes: r.value.bytes, mime: r.value.mime };
    },
    upload: (bytes, file, meta) => encryptAndUploadBytes(bytes, file, meta),
    uploadPoster: (bytes, mime) => encryptAndUploadBytes(bytes, { mime, name: undefined }),
    adoptTrayUpload: (id) => adoptTrayUpload(id),
    uploadPart: (part) => uploadTrayPart(part),
    partUploaded: (entry, index, ref) => unsentOutbox.partUploaded(entry.groupIdB64, entry.tempId, index, ref),
    partsDone: (entry, attachments) => unsentOutbox.partsDone(entry.groupIdB64, entry.tempId, attachments),
    uploaded: (entry, ref) =>
      unsentOutbox.update(entry.groupIdB64, entry.tempId, { attachments: [ref], endMedia: true }),
    compose: (entry, attachments) =>
      composeHeldEnvelope(
        {
          text: entry.text,
          attached: [...attachments],
          reply: entry.reply,
          ...(entry.forward === undefined ? {} : { forward: entry.forward }),
          ...(entry.content === undefined ? {} : { content: entry.content }),
        },
        entry.reply,
      ),
    publish: async (g, envelope) => {
      const r = await chat.sendMessage(auth.identitySecrets(), g.group, envelope);
      return { messageId: r.messageId, createdAt: r.createdAt };
    },
    file: async (entry, attachments, result, g) => {
      const own = ownKeyB64();
      const row = sentRowOf(entry, attachments, result, own);
      const s = auth.state();
      const account = s.kind === "session" ? s.account.accountId : null;
      if (account !== null) {
        const deviceKey = await getDeviceKey(account);
        if (deviceKey !== null) {
          const hex = groupIdHex(g.group.groupId);
          await updateMessageLog(account, deviceKey, (current) => {
            const groups = current?.groups ?? {};
            return {
              version: MESSAGE_LOG_VERSION,
              groups: { ...groups, [hex]: appendToGroupSlot(groups[hex], [row], 0) },
            };
          });
        }
      }
      // A view of the conversation that mounted while this ran is told.
      notifyHeldFlushed({ groupIdB64: g.groupIdB64, messages: [row] });
      // ADR-0092 — our own poll or checklist never comes back to us.
      if (entry.content !== undefined && own !== undefined) {
        await receiveContent(g.group, g.groupIdB64, result.messageId, result.createdAt, own, entry.content);
      }
    },
    sendVerdict: async (_entry, g) => {
      if (conversationKindOf(g.group) !== "chat") return "send";
      const roster = (): string[] => personKeysOf(g.group);
      const own = (): string | null => ownPersonKeyB64(chats.identity());
      let conversationHex: string;
      try {
        conversationHex = groupIdHex(lineage.conversationIdOf(g.group.groupId));
      } catch {
        conversationHex = groupIdHex(g.group.groupId);
      }
      return oneToOneSendVerdict({
        check: () =>
          checkPeerIdentityIfDue({
            conversationHex,
            peerKeyB64: oneToOnePeerKey(roster(), own()),
            check: (accountId, peer) => contacts.checkContactIdentity(accountId, peer),
          }),
        unrepairableKey: () =>
          oneToOneUnrepairableKey({
            rosterPersonKeysB64: roster(),
            ownPersonKeyB64: own(),
            supersededKeys: loadSupersededKeys(),
          }),
        heldAtMs: Date.now(),
      });
    },
    hold: (entry, attachments) => holdEntry(entry, attachments),
    holdForSuccessor: (entry, attachments, err) =>
      err instanceof HeldForSuccessor ? holdEntry(entry, attachments) : false,
    isNetwork: (err) => isNetworkSendFailure(err),
  };

  /** Hand an entry to the ADR-0073 held queue of its conversation. */
  function holdEntry(entry: UnsentEntry, attachments: readonly AttachmentRefBytes[]): boolean {
    {
      const g = chats.get(entry.groupIdB64);
      if (g === undefined) return false;
      let conversationHex: string;
      try {
        conversationHex = groupIdHex(lineage.conversationIdOf(g.group.groupId));
      } catch {
        return false;
      }
      heldSends.hold(
        conversationHex,
        {
          text: entry.text,
          attached: [...attachments],
          reply: entry.reply,
          replyGroupIdB64: entry.groupIdB64,
          ...(entry.forward === undefined ? {} : { forward: entry.forward }),
          ...(entry.content === undefined ? {} : { content: entry.content }),
        },
        Date.now(),
      );
      const s = auth.state();
      void persistHeldSends(s.kind === "session" ? s.account.accountId : null);
      return true;
    }
  }

  const runner = new UnsentRunner(unsentOutbox, (entry) => sendUnsentHeadless(entry, deps));

  // Section 7.2 — every return to Online sends every conversation's queue.
  onCleanup(runWhenServersAnswer({ run: async () => runner.runAll() }));

  // A conversation this device no longer holds takes its unsent messages with
  // it.
  onCleanup(
    membershipEnd.onGroupWiped((groupIdB64) => {
      void unsentOutbox.removeGroup(groupIdB64);
    }),
  );

  let stopLanding: (() => void) | null = null;
  /** The servers answered since this session landed. */
  let answeredSinceLanding = false;
  onCleanup(() => stopLanding?.());

  // The session landing: read the outbox back, then send once the servers
  // are known to answer (the phase reads Online before any request, so the
  // next answer is waited for).
  createEffect(() => {
    const s = auth.state();
    const account = s.kind === "session" ? s.account.accountId : null;
    untrack(() => {
      stopLanding?.();
      stopLanding = null;
      answeredSinceLanding = false;
      if (account === null) {
        unsentOutbox.reset();
        return;
      }
      let cancelled = false;
      stopLanding = (): void => {
        cancelled = true;
      };
      void unsentOutbox.restore(account).then(() => {
        if (cancelled) return;
        const stop = onServersAnswer(() => {
          stop();
          if (cancelled) return;
          answeredSinceLanding = true;
          runner.runAll();
        });
        stopLanding = (): void => {
          cancelled = true;
          stop();
        };
      });
    });
  });

  // A conversation loaded after the landing (the registry fills in after the
  // session lands) gets its pass then, while the servers answer.
  createEffect(() => {
    const count = chats.groups().length;
    if (count === 0) return;
    untrack(() => {
      if (answeredSinceLanding && connectionPhase() === "online") runner.runAll();
    });
  });

  return (
    <UnsentOutboxContext.Provider value={{ outbox: unsentOutbox, runner }}>
      {props.children}
    </UnsentOutboxContext.Provider>
  );
}
