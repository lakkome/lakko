// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `/g/:slug` — group join-ticket landing (ADR-0028 Phase 3, G-3).
 *
 * The inverse of admin-adds-by-username: a non-member opens an owner-minted
 * `https://lakko.me/g/<slug>` link, decodes + verifies the
 * `GroupInviteCapability`, and requests to join. "Request to join" posts an MLS
 * external-join proposal (ADR-0027) to the group journal; an owner/admin
 * approves it from their members panel; and — since ADR-0045 S7 — whichever
 * device is the acting committer then sweeps that approval into a commit and
 * posts the `WelcomeRendezvous` this page polls for and opens.
 *
 * That last step is why the wait is TWO-stage and the copy says so. Approval no
 * longer admits anyone by itself: it publishes an `Add` proposal, and ADR-0045
 * decision 2 gives the sweep that commits it no deadline, so a request can sit
 * approved-but-not-yet-admitted for as long as no candidate committer is
 * running. Promising immediacy here would be the optimistic-update lie the UX
 * spec forbids.
 *
 * The link conveys only the `group_id` — it authorizes nobody; the admin
 * approval gate is the security boundary. Public route (anonymous visitors land
 * here and are prompted to log in), mirroring the `/i/` contact-invite route.
 *
 * ## What this side is NOT told (ADR-0045 S7 residual 1)
 *
 * A sweep can exclude the approval-derived `Add` — `narrow_sweep_by_exclusion`
 * drops whatever refuses to build, and the commonest cause needs no adversary
 * at all (an expired KeyPackage, reachable because a request has no deadline).
 * This page is never told when that happens, and the reason is structural
 * rather than an omission: there is no artifact addressed to a non-member, and
 * the journal is unauthenticated — the DS pseudonym IS the `group_id` and there
 * is no auth on it — so any "your request was not carried" envelope would be a
 * forgeable denial primitive. Inference does not work either: an epoch
 * advancing with no rendezvous for us is byte-for-byte what *no admin has
 * approved yet* looks like, which is the ordinary state.
 *
 * So the wait copy states the truth about the wait, and the page offers the one
 * repair this side has: `Request again`, user-driven, which mints a fresh
 * KeyPackage. It never claims that is why.
 */
import { Show, createMemo, createResource, createSignal, onCleanup } from "solid-js";
import { A, useNavigate, useParams } from "@solidjs/router";
import { useAuth } from "../auth/AuthContext";
import { useChats } from "../chat/ChatContext";
import { useProfiles } from "../profile/ProfileContext";
import { ContactsClient } from "../contacts/client";
import { loadWasm } from "../wasm";
import { base64UrlEncodeNoPad } from "../auth/account-id";
// ADR-0045 S7 residual 1, the joiner's half. The copy and the one repair live
// in a plain module because `vitest.config.ts` cannot test a `.tsx` file.
import {
  JOIN_REQUEST_AGAIN,
  JOIN_WAIT_PRIMARY,
  JOIN_WAIT_SECONDARY,
} from "../chat/unapplied-requests";
import { paths } from "./paths";
import { PublicLayout } from "../ui/PublicLayout";
import { joinGroupAuthLinks } from "./join-group-auth";
import { recordConversationKind } from "../chat/conversation-kind-store";

type JoinPhase = "idle" | "requesting" | "waiting" | "joined" | "error";

interface DecodedTicket {
  /** The capability wire bytes — resolved from the ledger when the slug was a
   *  pointer, or the inline bytes directly. */
  wire: Uint8Array;
  /** Non-null only for the server-backed pointer form; drives the consume gate
   *  (ADR-0028 OQ1). */
  blobId: Uint8Array | null;
  fingerprint: string;
  valid: boolean;
}

export default function JoinGroup() {
  const params = useParams<{ slug: string }>();
  const auth = useAuth();
  const chats = useChats();
  const profiles = useProfiles();
  const navigate = useNavigate();

  // Canonical identity provider — the same wasm Identity every Group on this
  // device uses (mirrors Invite.tsx).
  const client = new ContactsClient(undefined, undefined, {
    identityProvider: () => chats.identity(),
  });

  // Resolve the slug (pointer => ledger fetch, inline => passthrough), then
  // decode + verify the capability in a memo-driven resource so the panel
  // renders the right state without imperative juggling. Re-runs on navigation.
  const slugSignal = createMemo(() => params.slug);
  const [decoded] = createResource(slugSignal, async (slug): Promise<DecodedTicket | null> => {
    try {
      const { capabilityWire, blobId } = await client.resolveGroupInviteSlug(slug);
      const wasm = await loadWasm();
      const cap = wasm.GroupInviteCapability.decodeAndVerify(capabilityWire);
      return {
        wire: capabilityWire,
        blobId,
        fingerprint: cap.issuerFingerprint(),
        valid: cap.isValidNow(BigInt(Date.now())),
      };
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("group-join: decode/verify failed", err);
      return null;
    }
  });

  const [phase, setPhase] = createSignal<JoinPhase>("idle");
  const [error, setError] = createSignal<string | null>(null);
  /** In flight, so `Request again` cannot be double-tapped into two journal
   *  rows. Separate from `phase`, which stays `waiting` across a re-request —
   *  going back through `requesting` would hide the wait copy and read as a
   *  restart of something that never stopped. */
  const [requesting, setRequesting] = createSignal(false);
  let pollTimer: number | undefined;
  onCleanup(() => {
    if (pollTimer !== undefined) clearInterval(pollTimer);
  });

  async function onRequest(ticket: DecodedTicket): Promise<void> {
    if (requesting()) return;
    setError(null);
    setRequesting(true);
    // A re-request keeps the `waiting` copy on screen; only the first one
    // passes through `requesting`.
    if (phase() === "idle") setPhase("requesting");
    // A re-request replaces the poll rather than adding a second one.
    if (pollTimer !== undefined) clearInterval(pollTimer);
    try {
      const { groupId, joinerSignatureKey } = await client.requestToJoinFromLink(
        auth.identitySecrets(),
        ticket.wire,
        Date.now(),
        {
          blobId: ticket.blobId,
          joinerAccountId: auth.identitySecrets()?.accountId,
        },
      );
      setPhase("waiting");
      // Poll the journal for the rendezvous. ADR-0045 S7: an admin's approve
      // publishes a proposal, and the rendezvous appears only once a committer
      // sweeps it — so this poll spans both stages and keeps running until a
      // Welcome arrives or the page unmounts.
      pollTimer = window.setInterval(() => {
        void (async () => {
          try {
            const group = await client.pollForJoinWelcome(
              auth.identitySecrets(),
              groupId,
              joinerSignatureKey,
            );
            if (group !== null) {
              if (pollTimer !== undefined) clearInterval(pollTimer);
              // Register the joined group so the chat route can find it, and
              // ADR-0048 §2 — publish this device's profile into it: the
              // members already there cannot learn the newcomer's name any
              // other way.
              const active = chats.add({ label: "Group", group });
              profiles.groupJoined(active.groupIdB64);
              // docs/ux/client-redesign.md section 3 — a conversation joined
              // through an invite link is a group whatever its size.
              {
                const s = auth.state();
                if (s.kind === "session") recordConversationKind(s.account.accountId, active.groupIdB64, "group");
              }
              setPhase("joined");
              navigate(paths.chat(base64UrlEncodeNoPad(group.groupId)));
            }
          } catch (err) {
            if (pollTimer !== undefined) clearInterval(pollTimer);
            setError(String(err));
            setPhase("error");
          }
        })();
      }, 3000);
    } catch (err) {
      setError(String(err));
      setPhase("error");
    } finally {
      setRequesting(false);
    }
  }

  return (
    <PublicLayout>
      <h1>Join a group</h1>
      <Show when={decoded.loading}>
        <p>Loading invite…</p>
      </Show>
      <Show when={!decoded.loading && (decoded() === null || decoded()?.valid === false)}>
        <p>This invite link is invalid or has expired.</p>
        <A href={paths.home}>Open Lakko</A>
      </Show>
      <Show when={!decoded.loading && decoded() !== null && decoded()?.valid === true}>
        <Show
          when={auth.state().kind === "session" && auth.identitySecrets() !== null}
          fallback={
            <>
              <p>You need an account to ask to join. Create one, or log in.</p>
              <div class="lk-actions">
                <A href={joinGroupAuthLinks(params.slug).signup}>
                  <button class="primary" type="button">
                    Sign up
                  </button>
                </A>
                <A href={joinGroupAuthLinks(params.slug).login}>
                  <button type="button">Log in</button>
                </A>
              </div>
            </>
          }
        >
          <p>
            Invited by: <code>{decoded()?.fingerprint}</code>
          </p>
          <Show when={phase() === "idle"}>
            <button type="button" class="primary" onClick={() => void onRequest(decoded()!)}>
              Request to join
            </button>
          </Show>
          <Show when={phase() === "requesting"}>
            <p>Sending request…</p>
          </Show>
          <Show when={phase() === "waiting"}>
            <p>{JOIN_WAIT_PRIMARY}</p>
            <p style={{ color: "var(--surface-muted)", "font-size": "0.85rem" }}>
              {JOIN_WAIT_SECONDARY}
            </p>
            <button
              type="button"
              disabled={requesting()}
              onClick={() => void onRequest(decoded()!)}
            >
              {JOIN_REQUEST_AGAIN}
            </button>
          </Show>
          <Show when={phase() === "joined"}>
            <p>Joined! Opening chat…</p>
          </Show>
          <Show when={phase() === "error"}>
            <p style={{ color: "var(--error)" }}>Failed: {error()}</p>
          </Show>
        </Show>
      </Show>
    </PublicLayout>
  );
}
