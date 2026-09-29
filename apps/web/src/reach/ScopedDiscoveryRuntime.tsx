// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0058 sections 4 and 5 - the session runtime that publishes what this
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
 */
import { createEffect, onCleanup, untrack, type JSX } from "solid-js";

import { useAuth } from "../auth/AuthContext";
import { base64Decode, base64Encode } from "../auth/account-id";
import { useChats } from "../chat/ChatContext";
import { ChatClient } from "../chat/client";
import { readIsActive } from "../chat/membership-ended";
import { isOneToOne, useProfiles } from "../profile/ProfileContext";
import { loadWasm } from "../wasm";
import {
  bundleTargets,
  decodePublication,
  encodePublication,
  everyoneAction,
  offerKey,
  offerNeedsRebuild,
  offerTargets,
  recordsDigest,
  termsToEvaluate,
  type PublicationState,
} from "./publication-model";
import { reachSession, reachSessionSignal } from "./reach-service";
import { loadReachRow, saveReachRow } from "./reach-store";
import { ScopedDiscoveryClient } from "./scoped-discovery";
import { loadIntroductions } from "./scoped-intake";
import { visibilitySetting } from "./visibility-store";

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
    onPersist: async () => {
      const s = auth.state();
      if (s.kind === "session") await chats.persistSnapshot(s.account.accountId);
    },
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
    const name = profiles.ownProfile().displayName;
    const username = profiles.ownUsername();

    /** OPRF outputs of the terms (cached; each new one costs a token). */
    const hashesOf = async (terms: readonly { key: string; evaluate: () => Promise<Uint8Array> }[]) => {
      const missing = termsToEvaluate(state, terms.map((t) => t.key));
      if (missing.length > 0) {
        const added: Record<string, string> = {};
        for (const t of terms) {
          if (!missing.includes(t.key)) continue;
          added[t.key] = base64Encode(await t.evaluate());
        }
        await save({ ...state, termHashes: { ...state.termHashes, ...added } });
      }
      return terms.map((t) => base64Decode(state.termHashes[t.key] ?? ""));
    };
    const nameTerms = (n: string | null): { key: string; evaluate: () => Promise<Uint8Array> }[] =>
      n === null
        ? []
        : wasm.nameSearchTerms(n).map((term) => ({
            key: `n:${term}`,
            evaluate: () => scoped.nameTermHash(term),
          }));

    // 1. Everyone (ADR-0058 section 5). The mailbox's `open` flag is the
    //    reach session's; this is the discovery half.
    const everyone = setting.mode === wasm.VisibilityMode.Everyone;
    const action = everyoneAction(state.everyone, everyone, name);
    if (action.kind === "publish") {
      const hashes = await hashesOf(nameTerms(action.name));
      await scoped.putVisibility(true, hashes);
      await save({ ...state, everyone: { public: true, name: action.name } });
    } else if (action.kind === "withdraw") {
      await scoped.putVisibility(false, []);
      await save({ ...state, everyone: { public: false, name: null } });
    }

    // 2. The introduction record (section 4), rebuilt when an input moved.
    const fof = wasm.allowsContactsOfContacts(setting);
    const key = offerKey(session.currentVersion("fof"), name, username);
    if (offerNeedsRebuild(state.offer, fof, key)) {
      const terms = [
        ...nameTerms(name),
        ...(username === null
          ? []
          : [{ key: `u:${username}`, evaluate: () => scoped.usernameHash(username) }]),
      ];
      if (terms.length > 0) {
        const oprfs = await hashesOf(terms);
        const grant = session.grant("fof");
        const accountId = base64Decode(account);
        const payload = new wasm.IntroductionPayload(
          session.accountKey,
          accountId,
          name ?? undefined,
          username ?? undefined,
          username === null ? undefined : (profiles.ownUsernameCertificate() ?? undefined),
          grant,
        );
        grant.free();
        try {
          const record = wasm.composeIntroduction(payload, oprfs, BigInt(Date.now()));
          await save({ ...state, offer: { key, recordB64: base64Encode(record) }, offerSentTo: {} });
        } finally {
          payload.free();
        }
      }
    }

    // One-to-one conversations with contacts that can still carry a message.
    const oneToOnes = chats
      .groups()
      .filter((g) => readIsActive(g.group) !== false && isOneToOne(g.group));
    const byId = new Map(oneToOnes.map((g) => [g.groupIdB64, g]));

    // 3. Offer the record to every contact not sent this version yet.
    const offer = state.offer;
    if (offer !== null) {
      const offerBytes = wasm.composeIntroductionOffer(base64Decode(offer.recordB64));
      for (const g of offerTargets(state, fof, [...byId.keys()])) {
        const entry = byId.get(g);
        if (entry === undefined) continue;
        await publisher.broadcastControl(secrets, entry.group, offerBytes);
        await save({ ...state, offerSentTo: { ...state.offerSentTo, [g]: offer.key } });
      }
    }

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
  }

  createEffect(() => {
    // Re-run on what the steps depend on.
    reachSessionSignal();
    visibilitySetting();
    profiles.ownProfile();
    profiles.ownUsername();
    chats.groups();
    untrack(trigger);
  });

  const onOnline = (): void => trigger();
  if (typeof window !== "undefined") window.addEventListener("online", onOnline);
  const tick = setInterval(trigger, TICK_MS);
  onCleanup(() => {
    clearInterval(tick);
    if (typeof window !== "undefined") window.removeEventListener("online", onOnline);
  });
  return <></>;
}
