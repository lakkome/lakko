// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0060 sections 4 and 5 - the session runtime that publishes what this
 * account can be found by: the Everyone publication at discovery, the
 * introduction record offered to contacts (contacts of contacts), and the
 * bundle of contacts' records relayed to the other contacts.
 *
 * Mounted inside `<ProfileProvider>` (it reads the own name and the username
 * certificate). Every step is idempotent against the encrypted `publication`
 * row, so a re-run after a reload sends nothing that already went out; a
 * failed step is retried at the next run (a setting or name change, a new
 * contact, coming back online, or the periodic tick). Decisions are in
 * `publication-model.ts`.
 *
 * An offer and a bundle go out once per conversation, so each run also asks
 * for what this device never received, and answers such an ask by sending
 * its own again (ADR-0060 section 4, "A device that holds nothing asks").
 */
import { runPublicationSteps } from "./publication-steps";
import { createEffect, onCleanup, untrack, type JSX } from "solid-js";

import { useAuth } from "../auth/AuthContext";
import { base64Decode, base64Encode } from "../auth/account-id";
import { useChats } from "../chat/ChatContext";
import { ChatClient, rosterOf } from "../chat/client";
import { readIsActive } from "../chat/membership-ended";
import { personKeysHex, resolveAccountKeyHex } from "../contacts/address-resolver";
import { loadAccepted } from "../contacts/local-store";
import { journalHex } from "../ds/access";
import { lastProfileRequestAt, recordProfileRequest } from "../profile/profile-request";
import { isOneToOne, useProfiles } from "../profile/ProfileContext";
import { accountIdBytes, NO_ACCOUNT_ID } from "../settings/contact-visibility";
import { loadWasm } from "../wasm";
import {
  askForMissingIntroductions,
  askResetsRecords,
  bundleTargets,
  decodePublication,
  encodePublication,
  evaluateMissingTermsInBatch,
  type BatchTerm,
  everyoneAction,
  everyoneKey,
  forgetSentRecords,
  mayOfferTo,
  offerKey,
  offerNeedsRebuild,
  offerTargets,
  oneToOnePeerHex,
  recordsDigest,
  type PublicationState,
} from "./publication-model";
import { heldClassesForPerson } from "./received-grants";
import { reachSession, reachSessionSignal, reachVersionsTick } from "./reach-service";
import { loadReachRow, saveReachRow } from "./reach-store";
import { ScopedDiscoveryClient } from "./scoped-discovery";
import { DiscoveryHttpError } from "../discovery/client";
import type { OprfPair } from "../discovery/oprf-pair";
import { introductionOffersTick, loadIntroductions, takeIntroductionAsks } from "./scoped-intake";
import { visibilitySetting } from "./visibility-store";
import { durableSnapshotHook, sessionAccountOf } from "../chat/persist-hooks";
import { subscribeOwnStateRestored } from "../profile/own-profile-sync";

/** How often the runtime re-checks without a trigger (a contact's offer that
 *  arrived, a new one-to-one). Chosen, not measured. */
const TICK_MS = 5 * 60 * 1000;

export function ScopedDiscoveryRuntime(): JSX.Element {
  const auth = useAuth();
  const chats = useChats();
  const profiles = useProfiles();
  const scoped = new ScopedDiscoveryClient(() => auth.identitySecrets()?.sessionToken ?? null);
  const publisher = new ChatClient(undefined, {
    identityProvider: () => chats.identity(),
    onPersist: durableSnapshotHook(chats, () => sessionAccountOf(auth.state())),
  });

  let running: Promise<void> | null = null;
  let again = false;

  function trigger(): void {
    if (running !== null) {
      again = true;
      return;
    }
    running = run()
      .catch((err: unknown) => {
        // eslint-disable-next-line no-console
        console.warn("[reach] publication step failed; retried at the next run", err instanceof Error ? err.message : "");
      })
      .finally(() => {
        running = null;
        if (again) {
          again = false;
          trigger();
        }
      });
  }

  async function run(): Promise<void> {
    const s = auth.state();
    const session = reachSession();
    const setting = visibilitySetting();
    const secrets = auth.identitySecrets();
    if (s.kind !== "session" || session === null || setting === null || secrets === null) return;
    const account = s.account.accountId;
    const wasm = await loadWasm();
    let state = decodePublication(await loadReachRow(account, "publication"));
    const save = async (next: PublicationState): Promise<void> => {
      state = next;
      await saveReachRow(account, "publication", encodePublication(next));
    };

    // One-to-one conversations with contacts that can still carry a message.
    const oneToOnes = chats
      .groups()
      .filter((g) => readIsActive(g.group) !== false && isOneToOne(g.group));
    const byId = new Map(oneToOnes.map((g) => [g.groupIdB64, g]));

    /** The other person of a conversation (account key hex), or `null`. */
    const peerOf = (group: Parameters<typeof rosterOf>[0]): string | null =>
      oneToOnePeerHex(
        rosterOf(group).map((m) => resolveAccountKeyHex(journalHex(m.accountKey ?? m.signaturePublicKey))),
        session.accountKeyHex,
      );

    // The asks that arrived: forget those conversations in the sent records,
    // so steps 3 and 4 below send the offer and the bundle there again.
    for (const ask of takeIntroductionAsks()) {
      const entry = byId.get(ask.groupIdB64);
      const peerHex = entry === undefined ? null : peerOf(entry.group);
      let senderHex: string | null = null;
      try {
        senderHex = resolveAccountKeyHex(journalHex(base64Decode(ask.senderKeyB64)));
      } catch {
        senderHex = null;
      }
      const now = Date.now();
      const resets = askResetsRecords({
        oneToOne: peerHex !== null,
        fromOtherParticipant: peerHex !== null && senderHex === peerHex,
        lastResetAtMs: state.introductionsResentAt[ask.groupIdB64],
        nowMs: now,
      });
      if (resets) await save(forgetSentRecords(state, ask.groupIdB64, now));
    }

    // What this device itself never received: asked for first, since it
    // needs no token and must not wait on the steps below.
    await askForMissingIntroductions({
      conversations: oneToOnes.map((g) => ({ groupIdB64: g.groupIdB64, peerHex: peerOf(g.group) })),
      held: await loadIntroductions(account),
      keysOfPerson: personKeysHex,
      holdsContactGrant: (keys) => heldClassesForPerson(session.receivedGrants(), keys).has("contact"),
      lastAskedAt: (g) => lastProfileRequestAt(account, g),
      recordAsked: (g, at) => recordProfileRequest(account, g, at),
      send: async (g) => {
        const entry = byId.get(g);
        if (entry === undefined) return;
        await publisher.broadcastControl(secrets, entry.group, wasm.composeProfileRequest());
      },
      rule: (joined, unnamed, last, now) => wasm.shouldRequestProfiles(joined, unnamed, last, now),
      now: () => Date.now(),
    });

    const name = profiles.ownProfile().displayName;
    const username = profiles.ownUsername();

    // Fetched here too: on a device that has written nothing yet (a restore,
    // a new account with no conversation) no profile has asked for it, and a
    // record with no certificate cannot show the username to a finder.
    const certificate = username === null ? null : await profiles.fetchOwnUsernameCertificate();

    /** A term to evaluate: its cache key and the OPRF round that yields it. */
    type Term = BatchTerm;

    /** The OPRF OUTPUTS of the terms (cached). The terms not yet kept go in
     *  ONE batch round (`POST /discovery/oprf/batch`, one register token):
     *  they are this account's own name words and username. Outputs never
     *  leave the device; see `lookupKeysOf`. */
    const outputsOf = async (terms: readonly Term[]): Promise<Uint8Array[]> => {
      const kept = await evaluateMissingTermsInBatch(state, terms, {
        evaluateBatch: async (missing) =>
          (await scoped.ownTermsOprf(missing)).map((pair: OprfPair) => base64Encode(pair.output)),
        keep: (termOutputs) => save({ ...state, termOutputs }),
        isRateLimited: (err) => err instanceof DiscoveryHttpError && err.status === 429,
        wait: (ms) => new Promise((resolve) => setTimeout(resolve, ms)),
      });
      return terms.map((t) => base64Decode(kept[t.key] ?? state.termOutputs[t.key] ?? ""));
    };
    /** The lookup keys discovery gets for these outputs (ADR-0060 §5). */
    const lookupKeysOf = (outputs: readonly Uint8Array[]): Uint8Array[] =>
      outputs.map((o) => wasm.discoveryLookupKey(o));
    const nameTerms = (n: string | null): Term[] =>
      n === null
        ? []
        : wasm.nameSearchTerms(n).map((term) => ({ key: `n:${term}`, kind: "term" as const, value: term }));
    const usernameTerms = (u: string | null): Term[] =>
      u === null ? [] : [{ key: `u:${u}`, kind: "username" as const, value: u }];

    const fof = wasm.allowsContactsOfContacts(setting);
    // Each step on its own (`publication-steps.ts`): the relay of the
    // contacts' records runs even when this device could not rebuild its own.
    await runPublicationSteps(
      [
        {
          name: "everyone",
          run: async () => {
          // 1. Everyone (ADR-0060 section 5). The mailbox's `open` flag is the
          //    reach session's; this is the discovery half: the lookup keys of the
          //    name terms, and the public record sealed under the OUTPUTS of the
          //    name terms plus the username.
          const everyone = setting.mode === wasm.VisibilityMode.Everyone;
          const pubKey = everyoneKey(
            name,
            username,
            certificate === null ? null : base64Encode(certificate),
            session.accountKeyHex,
          );
          const action = everyoneAction(state.everyone, everyone, pubKey);
          if (action.kind === "publish") {
            const names = nameTerms(name);
            const all = [...names, ...usernameTerms(username)];
            if (all.length === 0) {
              // A public record needs a term, and discovery refuses Everyone
              // without one: nothing to publish until a name or a username exists.
              // Said out loud: this person stays unfindable while it holds.
              // eslint-disable-next-line no-console
              console.warn("[reach] Everyone not published: no name and no username yet; this account cannot be found");
            } else {
              const outputs = await outputsOf(all);
              const nameLookupKeys = lookupKeysOf(outputs.slice(0, names.length));
              const payload = wasm.IntroductionPayload.forPublicRecord(
                session.accountKey,
                base64Decode(account),
                name ?? undefined,
                username ?? undefined,
                certificate ?? undefined,
                // ADR-0078 - the public record carries this account's mailbox
                // binding (not an identity-derived address), so a finder learns
                // where to write and verifies it against the key it pins.
                session.mailboxBinding(),
              );
              let publicRecord: Uint8Array;
              try {
                publicRecord = wasm.composePublicRecord(payload, outputs, BigInt(Date.now()));
              } finally {
                payload.free();
              }
              await scoped.putVisibility({ public: true, nameTermLookupKeys: nameLookupKeys, publicRecord });
              await save({ ...state, everyone: { public: true, key: pubKey } });
            }
          } else if (action.kind === "withdraw") {
            await scoped.putVisibility({ public: false });
            await save({ ...state, everyone: { public: false, key: null } });
          }
          },
        },
        {
          name: "own-record",
          run: async () => {
          // 2. The introduction record (section 4), rebuilt when an input moved.
          const key = offerKey(
            session.currentVersion("fof"),
            name,
            username,
            base64Encode(session.mailboxBinding()),
          );
          if (offerNeedsRebuild(state.offer, fof, key)) {
            const terms = [...nameTerms(name), ...usernameTerms(username)];
            if (terms.length > 0) {
              const outputs = await outputsOf(terms);
              const grant = session.grant("fof");
              const accountId = base64Decode(account);
              const payload = new wasm.IntroductionPayload(
                session.accountKey,
                accountId,
                name ?? undefined,
                username ?? undefined,
                certificate ?? undefined,
                grant,
              );
              grant.free();
              try {
                const record = wasm.composeIntroduction(payload, outputs, BigInt(Date.now()));
                await save({ ...state, offer: { key, recordB64: base64Encode(record) }, offerSentTo: {} });
              } finally {
                payload.free();
              }
            }
          }
          },
        },
        {
          name: "offer",
          run: async () => {
          // 3. Offer the record to every contact the setting allows who was not
          //    sent this version yet. An ask reaches this step like a new contact:
          //    a contact not allowed gets no offer for it either.
          const offer = state.offer;
          if (offer !== null) {
            const offerBytes = wasm.composeIntroductionOffer(base64Decode(offer.recordB64));
            const contacts = loadAccepted(account).map((c) => ({
              keyHex: journalHex(base64Decode(c.signaturePubkeyB64)),
              accountIdHex: c.accountIdHex ?? null,
            }));
            const allowsContact = (hex: string | null): boolean =>
              wasm.allowsContact(setting, (hex === null ? null : accountIdBytes(hex)) ?? NO_ACCOUNT_ID);
            const allowed = (g: string): boolean => {
              const entry = byId.get(g);
              const peer = entry === undefined ? null : peerOf(entry.group);
              return peer !== null && mayOfferTo(personKeysHex(peer), contacts, allowsContact);
            };
            for (const g of offerTargets(state, fof, [...byId.keys()], allowed)) {
              const entry = byId.get(g);
              if (entry === undefined) continue;
              await publisher.broadcastControl(secrets, entry.group, offerBytes);
              await save({ ...state, offerSentTo: { ...state.offerSentTo, [g]: offer.key } });
            }
          }
          },
        },
        {
          name: "relay",
          run: async () => {
          // 4. Relay the records our contacts offered, as one padded bundle, to
          //    every contact (a contact may receive its own record back; it is
          //    harmless and hides nothing the bundle size does not).
          const intros = await loadIntroductions(account);
          const records = Object.values(intros.offers).map((o) => o.recordB64);
          if (records.length > 0) {
            const digest = recordsDigest(records);
            const targets = bundleTargets(state, digest, [...byId.keys()]);
            if (targets.length > 0) {
              const bundle = wasm.composeIntroductionsBundle(
                records.map((r) => base64Decode(r)).slice(0, wasm.maxBundleRecords()),
                BigInt(Date.now()),
              );
              for (const g of targets) {
                const entry = byId.get(g);
                if (entry === undefined) continue;
                await publisher.broadcastControl(secrets, entry.group, bundle);
                await save({ ...state, bundleSentTo: { ...state.bundleSentTo, [g]: digest } });
              }
            }
          }
          },
        },
      ],
      (step, err) => {
        // eslint-disable-next-line no-console
        console.warn(`[reach] publication step ${step} failed; the next step runs`, err instanceof Error ? err.name : "");
      },
    );
  }

  createEffect(() => {
    // Re-run on what the steps depend on.
    reachSessionSignal();
    reachVersionsTick();
    introductionOffersTick();
    visibilitySetting();
    profiles.ownProfile();
    profiles.ownUsername();
    profiles.ownUsernameCertificate();
    chats.groups();
    untrack(trigger);
  });

  const onOnline = (): void => trigger();
  if (typeof window !== "undefined") window.addEventListener("online", onOnline);
  const tick = setInterval(trigger, TICK_MS);
  // A restore dropped the note of what was published.
  const unsubscribeRestored = subscribeOwnStateRestored(trigger);
  onCleanup(() => {
    unsubscribeRestored();
    clearInterval(tick);
    if (typeof window !== "undefined") window.removeEventListener("online", onOnline);
  });
  return <></>;
}
