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
 */
import { Show, createMemo, createResource, createSignal, onCleanup } from "solid-js";
import { A, useNavigate, useParams } from "@solidjs/router";
import { useAuth } from "../auth/AuthContext";
import { useChats } from "../chat/ChatContext";
import { ContactsClient } from "../contacts/client";
import { loadWasm } from "../wasm";
import { base64UrlEncodeNoPad } from "../auth/account-id";
import { paths } from "./paths";

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
  let pollTimer: number | undefined;
  onCleanup(() => {
    if (pollTimer !== undefined) clearInterval(pollTimer);
  });

  async function onRequest(ticket: DecodedTicket): Promise<void> {
    setError(null);
    setPhase("requesting");
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
    }
  }

  return (
    <article style={{ "max-width": "32rem", margin: "2rem auto", padding: "1rem" }}>
      <h1>Join group</h1>
      <Show when={decoded.loading}>
        <p>Loading invite…</p>
      </Show>
      <Show when={!decoded.loading && (decoded() === null || decoded()?.valid === false)}>
        <p>This invite link is invalid or has expired.</p>
        <A href={paths.home}>Go home</A>
      </Show>
      <Show when={!decoded.loading && decoded() !== null && decoded()?.valid === true}>
        <Show
          when={auth.state().kind === "session" && auth.identitySecrets() !== null}
          fallback={
            <>
              <p>Log in to request to join this group.</p>
              <A href={paths.login}>Log in</A>
            </>
          }
        >
          <p>
            Invited by: <code>{decoded()?.fingerprint}</code>
          </p>
          <Show when={phase() === "idle"}>
            <button type="button" onClick={() => void onRequest(decoded()!)}>
              Request to join
            </button>
          </Show>
          <Show when={phase() === "requesting"}>
            <p>Sending request…</p>
          </Show>
          <Show when={phase() === "waiting"}>
            <p>
              Request sent. Waiting for an admin to approve, then for the
              change to be applied to the group.
            </p>
          </Show>
          <Show when={phase() === "joined"}>
            <p>Joined! Opening chat…</p>
          </Show>
          <Show when={phase() === "error"}>
            <p style={{ color: "var(--error)" }}>Failed: {error()}</p>
          </Show>
        </Show>
      </Show>
    </article>
  );
}
