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
 * `bootstrap_referral` capabilities are flagged in the UI but
 * still go through the same Accept path in V1; the dedicated
 * "auto-send-on-signup" bootstrap flow lands with the mobile
 * apps in 1.14 / 1.15.
 *
 * The page is a **public route** — registered outside the
 * `<RequireSession>` block in `routes.tsx` — because an invite
 * URL can be opened by anyone, including users who haven't
 * authenticated yet on this device.
 */
import { createMemo, createResource, createSignal, Show, type JSX } from "solid-js";
import { A, useNavigate, useParams } from "@solidjs/router";
import type { InviteCapability as WasmInviteCapability } from "../../wasm/lakko_ffi_wasm";
import { useAuth } from "../auth/AuthContext";
import { ContactsClient, ContactsError } from "../contacts/client";
import {
  describeExpiry,
  describeMaxUses,
  parseInviteSlug,
} from "../contacts/invite";
import { DiscoveryClient } from "../discovery/client";
import { useChats } from "../chat/ChatContext";
import { loadWasm } from "../wasm";
import { paths } from "./paths";

interface DecodedInvite {
  cap: WasmInviteCapability;
  fingerprintShort: string;
}

export default function Invite(): JSX.Element {
  const params = useParams<{ slug: string }>();
  const auth = useAuth();
  const chats = useChats();
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
  const discovery = new DiscoveryClient();
  const [decoded] = createResource(slugSignal, async (slug): Promise<DecodedInvite | null> => {
    try {
      const parsed = parseInviteSlug(slug);
      let bytes: Uint8Array;
      if (parsed.kind === "pointer") {
        // 1.18d1b — fetch the bytes from /discovery/invite/{blob_id}.
        // 404 surfaces as `null` — same as the inline-form
        // decode failure below; the user-visible "invalid /
        // not found" panel is identical, by design (an
        // attacker cannot probe blob_ids by status code).
        const fetched = await discovery.fetchInvite(parsed.blobId);
        if (fetched === null) return null;
        bytes = fetched;
      } else {
        // 1.18c back-compat: capability bytes inline in the
        // URL itself, no server round-trip.
        bytes = parsed.capabilityBytes;
      }
      const wasm = await loadWasm();
      const cap = wasm.InviteCapability.decodeAndVerify(bytes);
      return {
        cap,
        fingerprintShort: shortFingerprintFor(cap.ownerSignaturePubkey),
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
  return (
    <section style={panelStyle()}>
      <h1 style={{ "margin-top": 0 }}>Sign up or log in to accept</h1>
      <p>
        This invite link needs a Lakko account to claim. Create one
        below — you'll come back to this page from your contacts list
        once you're in.
      </p>
      <div style={{ display: "flex", gap: "0.75rem", "margin-top": "1rem" }}>
        <A href={paths.signup}>
          <button class="primary" type="button">
            Sign up
          </button>
        </A>
        <A href={paths.login}>
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
  onDone: () => void;
}): JSX.Element {
  const auth = useAuth();
  const cap = props.invite.cap;
  const [intro, setIntro] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  const [notice, setNotice] = createSignal<string | null>(null);

  const nowMs = Date.now();
  const expired = !cap.isValidNow(BigInt(nowMs));
  const labelOrFingerprint = cap.label.length > 0 ? cap.label : `@${props.invite.fingerprintShort}`;

  async function onAccept(): Promise<void> {
    setError(null);
    setNotice(null);
    setBusy(true);
    try {
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
        <dt style={termStyle()}>Expiry</dt>
        <dd style={defStyle()}>{describeExpiry(cap.expiryMs, nowMs)}</dd>
        <dt style={termStyle()}>Uses allowed</dt>
        <dd style={defStyle()}>{describeMaxUses(cap.maxUses)}</dd>
        <Show when={cap.bootstrapReferral}>
          <dt style={termStyle()}>Type</dt>
          <dd style={defStyle()}>Referral link (signup-deep-link flag set)</dd>
        </Show>
      </dl>

      <Show when={expired}>
        <div class="form-error" role="alert">
          This invite has expired. Ask the inviter for a fresh link.
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
          disabled={expired || busy()}
        />
      </label>

      {error() !== null && <div class="form-error">{error()}</div>}
      {notice() !== null && <div class="form-notice">{notice()}</div>}

      <div style={{ display: "flex", gap: "0.5rem", "margin-top": "1rem" }}>
        <button
          class="primary"
          type="button"
          disabled={expired || busy()}
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
