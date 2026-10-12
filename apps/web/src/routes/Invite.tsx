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
import { A, useLocation, useNavigate, useParams } from "@solidjs/router";
import type { InviteCapability as WasmInviteCapability } from "../../wasm/lakko_ffi_wasm";
import { useAuth } from "../auth/AuthContext";
import { buildAuthUrlWithReturn } from "../auth/return-url";
import { ContactsClient } from "../contacts/client";
import { contactsErrorText } from "../contacts/errors";
import { establishedContactOfAccount, pinKeysOf } from "../contacts/known-contact-request";
import { accountKeysOf, loadAccepted } from "../contacts/local-store";
import { readVerifiedBinding } from "../contacts/verified-binding";
import { base64Decode, base64Encode, base64UrlEncodeNoPad } from "../auth/account-id";
import { groupIdHex } from "../devices/sync/state";
import { t } from "../l10n";
import { KeyedError } from "../l10n/keyed-error";
import { createLiveText } from "../l10n/live-text";
import { tRich } from "../l10n/rich";
import {
  INVITE_INLINE_MIN_LEN,
  describeExpiry,
  describeMaxUses,
} from "../contacts/invite";
import { DiscoveryClient, isDiscoveryRateLimited } from "../discovery/client";
import {
  INVITE_LINK_DAMAGED,
  INVITE_LINK_RATE_LIMITED,
  INVITE_LINK_TRY_AGAIN,
  inviteLinkWidths,
  isInviteLinkDamaged,
  openInviteLinkFromUrl,
} from "../contacts/invite-link";
import { useChats } from "../chat/ChatContext";
import { useProfiles } from "../profile/ProfileContext";
import { loadWasm } from "../wasm";
import { paths } from "./paths";
import { PublicLayout } from "../ui/PublicLayout";

interface DecodedInvite {
  cap: WasmInviteCapability;
  fingerprintShort: string;
  /** When the slug is an 8-character ledger id
   *  (1.18d1b), the joiner can call `/discovery/invite/{id}/consume`
   *  before sending the ContactRequest (1.18d2a). Inline-bytes
   *  URLs have no associated server row — back-compat
   *  compromise documented in the ROADMAP. The Accept path
   *  branches on this. */
  blobId: Uint8Array | null;
  /** The PLAINTEXT capability wire the sealed link opened to
   *  (`cap.wire`). 1.18d3c needs these for the per-joiner
   *  dedup-tag derivation: tag = HMAC(SHA-256(domain ‖
   *  capability_bytes), joiner_account_id). */
  capabilityBytes: Uint8Array;
}

/** The resource's answer when the ledger rate-limited this address. */
const RATE_LIMITED = "rate-limited" as const;
/** The resource's answer for a link that does not open under its fragment
 *  (sealed-link amendment of ADR-0028, 2026-09-28). */
const DAMAGED = "damaged" as const;

export default function Invite(): JSX.Element {
  const params = useParams<{ slug: string }>();
  const location = useLocation();
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
  // The fragment is the link secret (sealed-link amendment): re-run on it too.
  const slugSignal = createMemo(() => ({ slug: params.slug, hash: location.hash }));
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
  const [decoded, { refetch }] = createResource(
    slugSignal,
    async ({ slug, hash }): Promise<
      DecodedInvite | typeof RATE_LIMITED | typeof DAMAGED | null
    > => {
    try {
      const wasm = await loadWasm();
      // Sealed-link amendment of ADR-0028 (2026-09-28): a missing or
      // malformed fragment is refused before any request; the pointer's blob
      // (or the inline one) opens only under the fragment's secret, which
      // also verifies the capability inside. A 404 surfaces as `null`, the
      // same invalid panel as a malformed slug, by design.
      const opened = await openInviteLinkFromUrl({
        slug,
        hash,
        widths: inviteLinkWidths(wasm),
        inlineMinLen: INVITE_INLINE_MIN_LEN,
        what: "invite",
        fetchPointer: (blobId) => discovery.fetchInvite(blobId),
        open: (sealed, secret) => wasm.InviteCapability.openLink(sealed, secret),
      });
      if (opened.kind === "not-found") return null;
      const cap = opened.capability;
      return {
        cap,
        fingerprintShort: shortFingerprintFor(cap.ownerSignaturePubkey),
        blobId: opened.blobId,
        capabilityBytes: cap.wire,
      };
    } catch (err) {
      // A 429 says nothing about the link: say "try again later", never
      // "invalid" (ADR-0028 short-link amendment).
      if (isDiscoveryRateLimited(err)) return RATE_LIMITED;
      if (isInviteLinkDamaged(err)) return DAMAGED;
      // eslint-disable-next-line no-console
      console.warn("invite: decode/verify failed", err);
      return null;
    }
    },
  );
  const invite = (): DecodedInvite | null => {
    const d = decoded();
    return d === undefined || d === null || d === RATE_LIMITED || d === DAMAGED ? null : d;
  };

  // 1.18c-V1 wires only the canonical accessor (no persist hook
  // — `sendContactRequestToBundle` doesn't mutate MLS state, so
  // there's nothing to snapshot).
  const client = new ContactsClient(undefined, undefined, {
    identityProvider: () => chats.identity(),
    // ADR-0048 §2 — the request carries this account's profile.
    ownProfileEnvelope: () => profiles.ownProfileEnvelope(),
  });

  return (
    <PublicLayout>
      <Show when={decoded.loading}>
        <p class="form-info">{t("invite.link.loading")}</p>
      </Show>
      <Show when={!decoded.loading && decoded() === null}>
        <InvalidInvitePanel />
      </Show>
      <Show when={!decoded.loading && decoded() === DAMAGED}>
        <section class="lk-stack" style={{ gap: "12px" }}>
          <p>{INVITE_LINK_DAMAGED()}</p>
        </section>
      </Show>
      <Show when={!decoded.loading && decoded() === RATE_LIMITED}>
        <RateLimitedInvitePanel onRetry={() => void refetch()} />
      </Show>
      <Show when={!decoded.loading && invite() !== null}>
        <Show
          when={auth.state().kind === "session" && auth.identitySecrets() !== null}
          fallback={<NeedsAuthPanel slug={params.slug} hash={location.hash} />}
        >
          <ConnectPanel
            invite={invite() as DecodedInvite}
            client={client}
            discovery={discovery}
            onDone={() => navigate(paths.contacts)}
          />
        </Show>
      </Show>
    </PublicLayout>
  );
}

// ---------------------------------------------------------------
// Subcomponents
// ---------------------------------------------------------------

function RateLimitedInvitePanel(props: { onRetry: () => void }): JSX.Element {
  return (
    <section class="lk-stack" style={{ gap: "12px" }}>
      <p>{INVITE_LINK_RATE_LIMITED()}</p>
      <div class="lk-actions">
        <button class="primary" type="button" onClick={() => props.onRetry()}>
          {INVITE_LINK_TRY_AGAIN()}
        </button>
      </div>
      <p>
        <A href={paths.home}>{t("invite.go_home")}</A>
      </p>
    </section>
  );
}

function InvalidInvitePanel(): JSX.Element {
  return (
    <section class="lk-stack" style={{ gap: "12px" }}>
      <h1>{t("invite.contact.invalid_title")}</h1>
      <p>{t("invite.contact.invalid_body")}</p>
      <p>
        <A href={paths.home}>{t("invite.go_home")}</A>
      </p>
    </section>
  );
}

function NeedsAuthPanel(props: { slug: string; hash: string }): JSX.Element {
  // 1.18e — preserve the current `/i/:slug` URL across the
  // auth round-trip. `buildAuthUrlWithReturn` encodes the
  // current path as a `?return=` query that Signup.tsx /
  // Login.tsx read on success to bounce back here. Pre-1.18e
  // the joiner had to re-paste / re-click the invite URL after
  // signup — easy to lose when copy-pasting from email.
  // The fragment is the link secret; without it the link opens nothing
  // (sealed-link amendment), so the return path keeps it.
  const fragment =
    props.hash.length === 0 || props.hash.startsWith("#") ? props.hash : `#${props.hash}`;
  const returnTo = `${paths.invite(props.slug)}${fragment}`;
  return (
    <section class="lk-stack" style={{ gap: "12px" }}>
      <h1>{t("invite.contact.needs_auth_title")}</h1>
      <p>{t("invite.contact.needs_auth_body")}</p>
      <div class="lk-actions">
        <A href={buildAuthUrlWithReturn(paths.signup, returnTo)}>
          <button class="primary" type="button">
            {t("invite.sign_up")}
          </button>
        </A>
        <A href={buildAuthUrlWithReturn(paths.login, returnTo)}>
          <button type="button">{t("auth.login.submit")}</button>
        </A>
      </div>
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
  const navigate = useNavigate();
  const cap = props.invite.cap;
  const [intro, setIntro] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createLiveText();
  const [notice, setNotice] = createLiveText();
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
      // ADR-0065 R.2: the link's owner may already be a contact under other
      // keys. The published identity then passes the fail-closed gate before
      // a use of the link is spent or anything is sent (an attested reset is
      // accepted there, silently); a stranger passes.
      await props.client.refuseContactRequestToUnverifiedKeys(
        cap.ownerAccountId,
        cap.ownerSignaturePubkey,
      );
      // ADR-0065 R.7 case 1 / UX 16.6 - a link from a person who is already
      // a contact (a reset loses their contacts; they may hand out a new
      // link) opens the existing contact: no request is sent and no use of
      // the link is spent. The check above ran R.2 first, so an attested
      // reset was accepted there and an unproven change was refused.
      // "Already a contact" is the pin or a verified binding naming the
      // row's key: a row whose account id came only from a peer's own
      // profile does not swallow this person's link.
      const ownerAccountIdB64 = base64Encode(cap.ownerAccountId);
      const existing = establishedContactOfAccount(loadAccepted(), groupIdHex(cap.ownerAccountId), {
        pinKeysB64: pinKeysOf(accountKeysOf(ownerAccountIdB64)),
        bindingKeyB64: readVerifiedBinding(ownerAccountIdB64)?.keyB64 ?? null,
      });
      if (existing !== undefined) {
        navigate(paths.contact(base64UrlEncodeNoPad(base64Decode(existing.signaturePubkeyB64))));
        return;
      }
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
          throw new KeyedError("common.error.session_ended");
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
          setNotice(() => t("invite.contact.sending", { name: labelOrFingerprint }));
        }
      }

      const introBytes = new TextEncoder().encode(intro());
      // ADR-0060 section 6: the request is written to the owner's closed
      // mailbox under this link's `link` grant. Used for this one write and
      // freed; never kept.
      const linkGrant = cap.linkGrant;
      try {
        await props.client.sendContactRequestToBundle(
          auth.identitySecrets(),
          cap.ownerAccountId,
          cap.ownerSignaturePubkey,
          cap.ownerX25519Pubkey,
          introBytes,
          { writeGrant: linkGrant },
        );
      } finally {
        linkGrant.free();
      }
      setNotice(() => t("invite.contact.sent", { name: labelOrFingerprint }));
      // Brief pause so the user reads the notice; then navigate
      // to /contacts where the accepted-state UI lives.
      window.setTimeout(props.onDone, 1500);
    } catch (err) {
      // ADR-0054 S3: the consume spends an anonymous token; that failure,
      // like every other, renders from its kind (`contacts/errors.ts`).
      setError(
        isDiscoveryRateLimited(err)
          ? INVITE_LINK_RATE_LIMITED
          : contactsErrorText(err, "contacts.error.send", labelOrFingerprint),
      );
    } finally {
      setBusy(false);
    }
  }

  return (
    <section class="lk-stack" style={{ gap: "12px" }}>
      <h1>{t("invite.contact.title", { name: labelOrFingerprint })}</h1>
      <dl>
        <dt>{t("invite.contact.fingerprint")}</dt>
        <dd>
          <code>{props.invite.fingerprintShort}</code>
        </dd>
        <dt>{t("contacts.safety.number")}</dt>
        <dd>
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
            {tRich(
              "common.sentence_pair",
              { first: <strong>{t("invite.contact.check_title")}</strong> },
              { second: t("invite.contact.check_body") },
            )}
          </p>
        </dd>
        <dt>{t("invite.contact.expiry")}</dt>
        <dd>{describeExpiry(cap.expiryMs, nowMs)}</dd>
        <dt>{t("invite.contact.uses")}</dt>
        <dd>{describeMaxUses(cap.maxUses)}</dd>
        <Show when={cap.bootstrapReferral}>
          <dt>{t("invite.contact.type")}</dt>
          <dd>{t("invite.contact.referral")}</dd>
        </Show>
      </dl>

      <Show when={expired}>
        <div class="form-error" role="alert">
          {t("invite.contact.expired")}
        </div>
      </Show>

      <Show when={exhausted() === "exhausted"}>
        <div class="form-error" role="alert">
          {t("invite.contact.used_up")}
        </div>
      </Show>

      <Show when={exhausted() === "not-found"}>
        <div class="form-error" role="alert">
          {t("invite.contact.no_longer_valid")}
        </div>
      </Show>

      <Show when={exhausted() === "already-consumed"}>
        <div class="form-notice" role="status">
          {tRich("invite.contact.already_accepted", {
            name: <strong>{labelOrFingerprint}</strong>,
          })}
        </div>
      </Show>

      <label>
        {t("invite.contact.intro_label")}
        <input
          type="text"
          value={intro()}
          onInput={(e) => setIntro(e.currentTarget.value)}
          placeholder={t("contacts.add.intro_placeholder")}
          disabled={expired || busy() || exhausted() !== null}
        />
      </label>

      <Show when={error()}>{(message) => <div class="form-error">{message()}</div>}</Show>
      {notice() !== null && <div class="form-notice">{notice()}</div>}

      <div class="lk-actions">
        <button
          class="primary"
          type="button"
          disabled={expired || busy() || exhausted() !== null}
          onClick={() => {
            void onAccept();
          }}
        >
          {busy() ? t("common.sending") : t("contacts.request.accept")}
        </button>
        <button type="button" onClick={props.onDone} disabled={busy()}>
          {t("contacts.request.decline")}
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
