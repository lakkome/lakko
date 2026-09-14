// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `/i/:slug` — invite-link landing page (Phase 1.18c).
 *
 * The slug is the base64url-no-pad encoding of an
 * `InviteCapability` (see `crates/lakko-contacts::invite` for the
 * codec and `contacts/invite.ts` for the URL-side helpers).
 *
 * Three top-level states the page resolves to:
 *
 *   1. **Bad slug / decode failure** — the URL is malformed or
 *      the signature didn't verify. We render a generic
 *      "this invite link is invalid" panel; we deliberately
 *      do not surface the underlying error string (it would
 *      leak codec internals).
 *   2. **Anonymous user** — the visitor opened the link without
 *      a Lakko session. We render a "log in / sign up to
 *      accept" prompt. Deep-link return after auth is a 1.18c
 *      follow-up — for now the visitor manually re-opens the
 *      link after authenticating.
 *   3. **Authenticated user** — we render the inviter card
 *      (label / fingerprint / expiry / max-uses) and an
 *      Accept / Decline pair. Accept calls
 *      `ContactsClient.sendContactRequestToBundle(...)` with
 *      the capability's pre-resolved bundle (no
 *      `/discovery/lookup` round-trip needed).
 *
 * `bootstrap_referral` capabilities are flagged in the UI and
 * **auto-trigger Accept on mount** (Phase 1.9b4 — ADR-0005 §13.4).
 * The click on the referral link IS the consent — the joiner
 * does not need to press Accept again. Decline stays clickable
 * during the 1.5s navigate-back grace window. For not-yet-
 * signed-in joiners, the existing `?return=` round-trip through
 * Signup / Login (1.18e) brings them back to this page, where
 * the auto-accept then fires once their session is live.
 *
 * The page is a **public route** — registered outside the
 * `<RequireSession>` block in `routes.tsx` — because an invite
 * URL can be opened by anyone, including users who haven't
 * authenticated yet on this device.
 */
import {
  createMemo,
  createResource,
  createSignal,
  onMount,
  Show,
  type JSX,
} from "solid-js";
import { A, useNavigate, useParams } from "@solidjs/router";
import type { InviteCapability as WasmInviteCapability } from "../../wasm/lakko_ffi_wasm";
import { useAuth } from "../auth/AuthContext";
import { buildAuthUrlWithReturn } from "../auth/return-url";
import { ContactsClient, ContactsError } from "../contacts/client";
import {
  describeExpiry,
  describeMaxUses,
  parseInviteSlug,
} from "../contacts/invite";
import { DiscoveryClient } from "../discovery/client";
import { useChats } from "../chat/ChatContext";
import { useProfiles } from "../profile/ProfileContext";
import { loadWasm } from "../wasm";
import { paths } from "./paths";

interface DecodedInvite {
  cap: WasmInviteCapability;
  fingerprintShort: string;
  /** When the slug is a 32-byte content-addressed pointer
   *  (1.18d1b), the joiner can call `/discovery/invite/{id}/consume`
   *  before sending the ContactRequest (1.18d2a). Inline-bytes
   *  URLs have no associated server row — back-compat
   *  compromise documented in the ROADMAP. The Accept path
   *  branches on this. */
  blobId: Uint8Array | null;
  /** Raw capability wire bytes (post `fetchInvite` decode for
   *  the pointer path; the URL slug bytes for the inline
   *  back-compat path). 1.18d3c needs these for the per-joiner
   *  dedup-tag derivation: tag = HMAC(SHA-256(domain ‖
   *  capability_bytes), joiner_account_id). */
  capabilityBytes: Uint8Array;
}

export default function Invite(): JSX.Element {
  const params = useParams<{ slug: string }>();
  const auth = useAuth();
  const chats = useChats();
  const profiles = useProfiles();
  const navigate = useNavigate();

  // Decode the slug + verify the capability signature in a
  // memo-driven async resource so the panel below renders the
  // appropriate state (loading / invalid / decoded) without any
  // imperative juggling. Source signal is the raw slug —
  // re-runs on navigation, no stale capability across
  // /i/<a> → /i/<b> transitions.
  const slugSignal = createMemo(() => params.slug);
  // Used for the pointer-form path. Created once per route
  // mount; reuses the existing fetch defaults (Vite proxy in
  // dev, same-origin nginx in prod).
  // Phase 1.5b: the joiner-side fetchInvite / consumeInvite calls
  // don't strictly require auth (Universal-Link joiners may not
  // be signed up yet — fetch + consume are open by design), but
  // we thread the token when available so logged-in joiners get
  // the standard Authorization header on every protected path
  // the page might trigger later.
  const discovery = new DiscoveryClient();
  discovery.setSessionToken(auth.identitySecrets()?.sessionToken ?? null);
  const [decoded] = createResource(slugSignal, async (slug): Promise<DecodedInvite | null> => {
    try {
      const parsed = parseInviteSlug(slug);
      let bytes: Uint8Array;
      let blobId: Uint8Array | null;
      if (parsed.kind === "pointer") {
        // 1.18d1b — fetch the bytes from /discovery/invite/{blob_id}.
        // 404 surfaces as `null` — same as the inline-form
        // decode failure below; the user-visible "invalid /
        // not found" panel is identical, by design (an
        // attacker cannot probe blob_ids by status code).
        const fetched = await discovery.fetchInvite(parsed.blobId);
        if (fetched === null) return null;
        bytes = fetched;
        blobId = parsed.blobId;
      } else {
        // 1.18c back-compat: capability bytes inline in the
        // URL itself, no server round-trip. No blob_id — the
        // /consume path (1.18d2a) is skipped on Accept.
        bytes = parsed.capabilityBytes;
        blobId = null;
      }
      const wasm = await loadWasm();
      const cap = wasm.InviteCapability.decodeAndVerify(bytes);
      return {
        cap,
        fingerprintShort: shortFingerprintFor(cap.ownerSignaturePubkey),
        blobId,
        capabilityBytes: bytes,
      };
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("invite: decode/verify failed", err);
      return null;
    }
  });

  // 1.18c-V1 wires only the canonical accessor (no persist hook
  // — `sendContactRequestToBundle` doesn't mutate MLS state, so
  // there's nothing to snapshot).
  const client = new ContactsClient(undefined, undefined, {
    identityProvider: () => chats.identity(),
    // ADR-0048 §2 — the request carries this account's profile.
    ownProfileEnvelope: () => profiles.ownProfileEnvelope(),
  });

  return (
    <article style={pageStyle()}>
      <Show when={decoded.loading}>
        <section style={panelStyle()}>
          <p>Loading invite…</p>
        </section>
      </Show>
      <Show when={!decoded.loading && decoded() === null}>
        <InvalidInvitePanel />
      </Show>
      <Show when={!decoded.loading && decoded() !== null}>
        <Show
          when={auth.state().kind === "session" && auth.identitySecrets() !== null}
          fallback={<NeedsAuthPanel slug={params.slug} />}
        >
          <ConnectPanel
            invite={decoded() as DecodedInvite}
            client={client}
            discovery={discovery}
            onDone={() => navigate(paths.contacts)}
          />
        </Show>
      </Show>
    </article>
  );
}

// ---------------------------------------------------------------
// Subcomponents
// ---------------------------------------------------------------

function InvalidInvitePanel(): JSX.Element {
  return (
    <section style={panelStyle()}>
      <h1 style={{ "margin-top": 0 }}>This invite link is invalid</h1>
      <p>
        The URL is malformed, the capability signature did not verify, or
        the capability bytes have been tampered with. Ask the inviter
        for a fresh link.
      </p>
      <p>
        <A href={paths.home}>Go to Lakko home</A>
      </p>
    </section>
  );
}

function NeedsAuthPanel(props: { slug: string }): JSX.Element {
  // 1.18e — preserve the current `/i/:slug` URL across the
  // auth round-trip. `buildAuthUrlWithReturn` encodes the
  // current path as a `?return=` query that Signup.tsx /
  // Login.tsx read on success to bounce back here. Pre-1.18e
  // the joiner had to re-paste / re-click the invite URL after
  // signup — easy to lose when copy-pasting from email.
  const returnTo = paths.invite(props.slug);
  return (
    <section style={panelStyle()}>
      <h1 style={{ "margin-top": 0 }}>Sign up or log in to accept</h1>
      <p>
        This invite link needs a Lakko account to claim. Create one
        below — you'll come back here automatically after sign-up.
      </p>
      <div style={{ display: "flex", gap: "0.75rem", "margin-top": "1rem" }}>
        <A href={buildAuthUrlWithReturn(paths.signup, returnTo)}>
          <button class="primary" type="button">
            Sign up
          </button>
        </A>
        <A href={buildAuthUrlWithReturn(paths.login, returnTo)}>
          <button type="button">Log in</button>
        </A>
      </div>
      <p style={{ color: "var(--surface-muted)", "font-size": "0.85rem", "margin-top": "1rem" }}>
        Slug: <code>{props.slug.slice(0, 16)}…</code>
      </p>
    </section>
  );
}

function ConnectPanel(props: {
  invite: DecodedInvite;
  client: ContactsClient;
  discovery: DiscoveryClient;
  onDone: () => void;
}): JSX.Element {
  const auth = useAuth();
  const cap = props.invite.cap;
  const [intro, setIntro] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  const [notice, setNotice] = createSignal<string | null>(null);
  // 1.18d2a-web: when the server reports 410 Gone (or 404 on
  // /consume — same fold-pattern as fetch), we lock the panel
  // into a "this invite has been used up / revoked" state and
  // disable Accept. Decline still works; the user can navigate
  // back to /contacts.
  // 1.18d3c: `already-consumed` is a fourth terminal state —
  // the joiner has accepted this invite before, so we lock the
  // panel + skip the ContactRequest send (re-sending would
  // land another envelope in the inviter's stranger inbox;
  // bad UX even if the server idempotency means it's safe).
  const [exhausted, setExhausted] = createSignal<
    null | "exhausted" | "not-found" | "already-consumed"
  >(null);

  const nowMs = Date.now();
  const expired = !cap.isValidNow(BigInt(nowMs));
  // ADR-0048 — a fingerprint is never shown with `@`: it is not a username.
  const labelOrFingerprint = cap.label.length > 0 ? cap.label : props.invite.fingerprintShort;
  // 1.9b4: bootstrap-referral auto-accept. When the inviter
  // flagged the capability as a referral signup-deep-link
  // (ADR-0005 §13.4), the joiner does not need to press Accept
  // manually — the click on the link IS the consent. We
  // auto-trigger `onAccept` once the panel mounts in a usable
  // state (not expired, not already exhausted, identity secrets
  // present). The user can still Decline before the inner
  // `onAccept` round-trip lands, since we surface a "Sending…"
  // notice and the Decline button remains active. The browser-
  // side navigation back to /contacts also gives a 1.5s grace
  // window for second thoughts.
  //
  // The empty intro is intentional — a bootstrap-referral click
  // is by construction a "we already know each other" gesture;
  // the inviter doesn't expect an introduction.
  onMount(() => {
    if (!cap.bootstrapReferral) return;
    if (expired) return;
    if (auth.identitySecrets() === null) return;
    // Pre-flight all the same gates `Accept` checks; let the
    // click-driven path handle the actual server round-trip /
    // error surfacing so we don't duplicate state-machine code.
    void onAccept();
  });

  async function onAccept(): Promise<void> {
    setError(null);
    setNotice(null);
    setBusy(true);
    try {
      // 1.18d2a-web: server-side max_uses enforcement runs on
      // the **pointer** path. Inline-bytes URLs have no
      // associated server row to bump — back-compat compromise
      // documented in ROADMAP 1.18d2a-web. We deliberately do
      // NOT silently fail-open on the inline path; it just
      // bypasses the ceiling, same as a 1.18c client would.
      // 1.18d3c: also drives the per-joiner dedup tag (HMAC
      // local) so a repeat from this joiner is caught
      // server-side and surfaced as `already-consumed`.
      if (props.invite.blobId !== null) {
        const secrets = auth.identitySecrets();
        if (secrets === null) {
          // The Show wrapper above guards against this, but
          // typescript wants the explicit check before reading
          // `secrets.accountId`.
          throw new Error("re-enter your password to accept invites");
        }
        const result = await props.discovery.consumeInvite(
          props.invite.blobId,
          props.invite.capabilityBytes,
          secrets.accountId,
        );
        if (result.kind === "exhausted") {
          setExhausted("exhausted");
          return;
        }
        if (result.kind === "not-found") {
          // Server's expiry filter folds expired into 404, so a
          // not-found here means either the inviter revoked
          // (1.18d2b) or expiry landed between page load and
          // consume. Either way the user-visible copy is the
          // same: this invite is no longer valid.
          setExhausted("not-found");
          return;
        }
        if (result.kind === "already-consumed") {
          // Joiner has consumed this invite before; server
          // returned the existing counter without bumping. We
          // skip the ContactRequest send (a fresh CR would
          // duplicate in the inviter's inbox) and surface a
          // clear "you've already accepted this" state.
          setExhausted("already-consumed");
          return;
        }
        // result.kind === "consumed" — fall through to send.
        if (
          result.maxUses !== 0xffff_ffff &&
          result.usedCount >= effectiveMaxUses(result.maxUses)
        ) {
          // This was the LAST use. We still send the
          // ContactRequest, but surface the milestone in the
          // notice copy below.
          setNotice(
            `Last use claimed — sending ContactRequest to ${labelOrFingerprint}…`,
          );
        }
      }

      const introBytes = new TextEncoder().encode(intro());
      const result = await props.client.sendContactRequestToBundle(
        auth.identitySecrets(),
        cap.ownerAccountId,
        cap.ownerSignaturePubkey,
        cap.ownerX25519Pubkey,
        introBytes,
      );
      setNotice(
        `ContactRequest sent to ${labelOrFingerprint} (DS id ${result.messageId}).`,
      );
      // Brief pause so the user reads the notice; then navigate
      // to /contacts where the accepted-state UI lives.
      window.setTimeout(props.onDone, 1500);
    } catch (err) {
      setError(formatError(err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <section style={panelStyle()}>
      <h1 style={{ "margin-top": 0 }}>Connect with {labelOrFingerprint}?</h1>
      <dl style={{ display: "grid", "grid-template-columns": "auto 1fr", gap: "0.4rem 1rem", margin: "1rem 0" }}>
        <dt style={termStyle()}>Inviter fingerprint</dt>
        <dd style={defStyle()}>
          <code>{props.invite.fingerprintShort}</code>
        </dd>
        <dt style={termStyle()}>Safety number</dt>
        <dd style={defStyle()}>
          <code
            style={{
              "font-size": "0.9rem",
              "letter-spacing": "0.02em",
              "word-spacing": "0.3em",
              "user-select": "all",
            }}
          >
            {cap.ownerFingerprint}
          </code>
          <p
            style={{
              "margin-top": "0.4rem",
              "margin-bottom": 0,
              "font-size": "0.85rem",
              opacity: 0.75,
            }}
          >
            <strong>Verify out-of-band before accepting.</strong> Ask
            the inviter for their safety number through a channel you
            already trust (Signal, in person, phone). A mismatch
            means this link is from someone else — typically a
            substitution attack where an attacker swapped the URL.
          </p>
        </dd>
        <dt style={termStyle()}>Expiry</dt>
        <dd style={defStyle()}>{describeExpiry(cap.expiryMs, nowMs)}</dd>
        <dt style={termStyle()}>Uses allowed</dt>
        <dd style={defStyle()}>{describeMaxUses(cap.maxUses)}</dd>
        <Show when={cap.bootstrapReferral}>
          <dt style={termStyle()}>Type</dt>
          <dd style={defStyle()}>
            Referral link — your ContactRequest auto-sends on
            arrival; no intro needed.
          </dd>
        </Show>
      </dl>

      <Show when={expired}>
        <div class="form-error" role="alert">
          This invite has expired. Ask the inviter for a fresh link.
        </div>
      </Show>

      <Show when={exhausted() === "exhausted"}>
        <div class="form-error" role="alert">
          This invite has been used up — its <code>max_uses</code> ceiling
          was reached. Ask the inviter for a fresh link.
        </div>
      </Show>

      <Show when={exhausted() === "not-found"}>
        <div class="form-error" role="alert">
          This invite is no longer valid — the inviter may have revoked it,
          or it expired between page load and click. Ask for a fresh link.
        </div>
      </Show>

      <Show when={exhausted() === "already-consumed"}>
        <div class="form-notice" role="status">
          You've already accepted this invite — your ContactRequest is
          on its way to <strong>{labelOrFingerprint}</strong>. No need
          to send another one. Check your <strong>Contacts</strong>
          page for the response.
        </div>
      </Show>

      <label style={{ display: "block", "margin-top": "1rem" }}>
        <span>Intro message (optional)</span>
        <input
          type="text"
          value={intro()}
          onInput={(e) => setIntro(e.currentTarget.value)}
          placeholder="hey, this is alice"
          style={{ width: "100%", "margin-top": "0.25rem" }}
          disabled={expired || busy() || exhausted() !== null}
        />
      </label>

      {error() !== null && <div class="form-error">{error()}</div>}
      {notice() !== null && <div class="form-notice">{notice()}</div>}

      <div style={{ display: "flex", gap: "0.5rem", "margin-top": "1rem" }}>
        <button
          class="primary"
          type="button"
          disabled={expired || busy() || exhausted() !== null}
          onClick={() => {
            void onAccept();
          }}
        >
          {busy() ? "Sending…" : "Accept"}
        </button>
        <button type="button" onClick={props.onDone} disabled={busy()}>
          Decline
        </button>
      </div>
    </section>
  );
}

/** Map the wire `max_uses` sentinels to the effective ceiling
 *  the server enforces. Mirrors `effective_max_uses` in
 *  `services/discovery/src/invite.rs` and the SQL CASE
 *  expression in the consume UPDATE. Drift between this and
 *  the server is a real-world bug worth pinning — but the
 *  function is tiny enough that the unit-test coverage on the
 *  server side (`effective_max_uses_maps_sentinels`) is
 *  sufficient. Used only to decide between "consumed
 *  successfully" and "this was the last use" UI copy. */
function effectiveMaxUses(maxUses: number): number {
  if (maxUses === 0) return 1; // SINGLE_USE
  if (maxUses === 0xffff_ffff) return 0xffff_ffff; // unlimited
  return maxUses;
}

// ---------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------

/** 12-hex-char prefix of the Ed25519 pubkey — same shape as
 *  `contacts/local-store.ts::fingerprintShort`, but inlined here
 *  because that helper takes a Uint8Array and we already have
 *  one. Cheap to keep consistent: a future extraction into a
 *  shared helper is mechanical. */
function shortFingerprintFor(pubkey: Uint8Array): string {
  let hex = "";
  for (let i = 0; i < pubkey.length && hex.length < 12; i++) {
    hex += (pubkey[i] ?? 0).toString(16).padStart(2, "0");
  }
  return hex.slice(0, 12);
}

function pageStyle(): JSX.CSSProperties {
  return {
    "max-width": "32rem",
    margin: "0 auto",
    padding: "1rem",
  };
}

function panelStyle(): JSX.CSSProperties {
  return {
    background: "var(--surface-elevated)",
    border: "1px solid var(--surface-border)",
    "border-radius": "var(--radius-md)",
    padding: "1.25rem",
  };
}

function termStyle(): JSX.CSSProperties {
  return {
    color: "var(--surface-muted)",
    "font-size": "0.85rem",
  };
}

function defStyle(): JSX.CSSProperties {
  return {
    margin: 0,
  };
}

function formatError(err: unknown): string {
  if (err instanceof ContactsError) return err.message;
  if (err instanceof Error) return err.message;
  return String(err);
}
