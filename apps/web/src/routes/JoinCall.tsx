// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `/c/:slug` — multi-joiner call-invite landing (ADR-0032, G-21/G-22).
 *
 * Whoever opens a `/c/<slug>` call link lands here. The screen decodes +
 * verifies the `CallInviteCapability` (owner's Ed25519 signature), then
 * drives the ADR-0032 owner-founds flow via {@link MultiJoinerCall}:
 *
 *   * If the visitor IS the owner (its identity signature key matches the
 *     capability's owner key), it FOUNDS + hosts the call
 *     ({@link MultiJoinerCall.host}). Opening your own link starts hosting.
 *   * Otherwise it JOINS: mint a fresh KeyPackage, connect to the SFU room,
 *     relay the KeyPackage to the acting committer, and wait for the Welcome
 *     ({@link MultiJoinerCall.join}). Until the Welcome lands the screen
 *     shows a "waiting for the owner to admit you" state (invariant #4 — no
 *     media before the group is joined). If no committer responds within the
 *     admission window the call is dead (owner left / call ended).
 *
 * ## Guest join (no login required, ADR-0032, docs/ux/call-links.md section 9)
 *
 * A visitor with no session joins without an account of their own making: on
 * `Join` a guest account is created silently (`auth.signupGuest()`, the
 * signup machinery without a username) and the call starts as soon as its
 * session lands. No recovery phrase is shown first: a person who was sent a
 * link wants the call. Once the call is over this page asks whether to keep
 * the account (`GuestAccountOffer`): `Create account` takes a username and a
 * first password (no phrase is shown; the "Protect your account" step
 * follows, as after a sign-up); `No, thanks` deletes the account at once. An account left unanswered is deleted by the servers after a day.
 *
 * ## The call belongs to the app-level call host
 *
 * This screen used to own the `MultiJoinerCall`, and its cleanup hung it up —
 * leaving the route ended the call for this device, and a link call could start
 * while a 1x1 or group call was live, both driving one in-call UI. The call now
 * lives in `calls/CallHostContext.tsx` ({@link CallHostApi.startLinkCall}): the
 * in-call overlay, audio, controls and hang-up are the host's and work on every
 * screen; returning here shows the host's state for this call. This screen
 * decodes the link, runs the guest sign-up, and asks. While another call holds
 * the one call slot the request is refused with the reason, and the user can
 * end that call from here.
 *
 * Public route, mirroring the `/i/` contact-invite + `/g/` group-join routes.
 */
import { accountSuspended } from "../auth/suspension-state";
import {
  Match,
  Show,
  Switch,
  createEffect,
  createMemo,
  createResource,
  createSignal,
  on,
  onCleanup,
  type JSX,
} from "solid-js";
import { A, useLocation, useNavigate, useParams } from "@solidjs/router";

import { useAuth } from "../auth/AuthContext";
import { Avatar } from "../ui/Avatar";
import { useChats } from "../chat/ChatContext";
import { useProfiles } from "../profile/ProfileContext";
import { createDisplayNameCheck } from "../profile/username-field";
import { maySaveDisplayName, savedDisplayName } from "../profile/rules";
import SignupPhrasePrompt from "../auth/SignupPhrasePrompt";
import { GuestAccountOffer } from "../auth/GuestAccountOffer";
import { isAbortError } from "../auth/pow";
import { signupErrorMessage } from "../auth/auth-error-copy";
import { guestWorkingTitle, signupWorkingLine } from "../auth/signup-working";
import { loadWasm } from "../wasm";
import { DiscoveryClient, isDiscoveryRateLimited } from "../discovery/client";
import {
  INVITE_LINK_DAMAGED,
  INVITE_LINK_OPEN_FAILED,
  INVITE_LINK_RATE_LIMITED,
  INVITE_LINK_TRY_AGAIN,
  inviteLinkWidths,
  isInviteLinkDamaged,
  isInviteLinkUnreachable,
  openInviteLinkFromUrl,
} from "../contacts/invite-link";
import { useCallHost, type CallHostApi } from "../calls/CallHostContext";
import { callIdHex } from "../calls/call-signaling-dispatch";
import { hexKey } from "../calls/multi-joiner-election";
import {
  linkCallOccupiesSlot,
  linkCallPagePhase,
  linkCallPageReturnsToApp,
  type LinkCallPhase,
} from "../calls/link-call-host";
import { fetchSfuRoomState } from "../calls/sfu-call-state";
import { sfuBaseUrl } from "../calls/sfu-config";
import { linkJoinRefusal, showsGuestQuestion } from "../calls/link-join-gate";
import { isSFrameTransformSupported } from "../calls/sframe-transform";
import { paths } from "./paths";
import { PublicLayout } from "../ui/PublicLayout";
import { t } from "../l10n";
import { createLiveText } from "../l10n/live-text";

/** What this screen shows: its own pre-call states (`idle`, and `starting`
 *  while a guest account is being created) until the host holds this call,
 *  then the host's phase for it. */
type Phase = "idle" | LinkCallPhase;

interface DecodedCall {
  /** Decoded + verified capability fields the flow needs. */
  readonly callId: Uint8Array;
  readonly ownerAccountId: Uint8Array;
  readonly ownerSignatureKey: Uint8Array;
  readonly ownerFingerprint: string;
  readonly mediaVideo: boolean;
  readonly valid: boolean;
  /** `true` when the local identity IS the capability owner. Always false
   *  for an anonymous visitor (they cannot be the owner). */
  readonly isOwner: boolean;
}

export default function JoinCall() {
  const params = useParams<{ slug: string }>();
  const auth = useAuth();
  const chats = useChats();
  const profiles = useProfiles();
  /** The host by name, when this device knows one — a contact or a received
   *  profile. Never a fingerprint: a string of digits names nobody, and the
   *  owner's own page has nothing to say about its host. */
  const hostName = (): string | null => {
    const d = decoded();
    if (d === null || d === undefined || d.isOwner) return null;
    const resolved = profiles.resolve(d.ownerSignatureKey);
    return resolved.source === "fingerprint" ? null : resolved.name;
  };
  /** The host's signature key, standard base64 (empty when unknown). */
  const hostKeyB64 = (): string => {
    const d = decoded();
    if (d === null || d === undefined) return "";
    let bin = "";
    for (const b of d.ownerSignatureKey) bin += String.fromCharCode(b);
    return btoa(bin);
  };
  const callHost: CallHostApi = useCallHost();

  // docs/ux/call-links.md section 9.1 — a guest is asked their name here,
  // prefilled with a NEW "<adjective> <animal>" from the shared rule
  // (`lakko_ui_rules::guest_name`) every time a link page opens (a new link
  // in the same tab included), never the name of an earlier visit. It is the
  // guest's name for the call (`docs/ux/calls.md` section 4.1); a brand-new
  // guest account also takes it as its profile name. The prefill lands once
  // the core has loaded, and never over a name the visitor has started
  // typing on this link.
  const [guestName, setGuestName] = createSignal("");
  let guestNameEdited = false;
  createEffect(
    on(
      () => params.slug,
      () => {
        guestNameEdited = false;
        setGuestName("");
        void loadWasm().then((wasm) => {
          if (guestNameEdited) return;
          const random = new Uint32Array(1);
          crypto.getRandomValues(random);
          setGuestName(wasm.guestName(random[0] ?? 0));
        });
      },
    ),
  );
  const guestNameCheck = createDisplayNameCheck(guestName);
  /** The name to save once the guest session lands; `undefined` = nothing to
   *  save (the visitor already had a session). */
  let pendingGuestName: string | null | undefined;
  const isAnonymousVisitor = (): boolean =>
    chats.identity() === null || auth.identitySecrets() === null;
  /** Whether the page asks for a name: a visitor who will join as a new
   *  guest, and a guest whose guest account from an earlier link is still on
   *  this device (section 9.1, amended 2026-11-07: before, that guest saw no
   *  field and kept the first random name for every later call). Never the
   *  link's owner. */
  const asksGuestName = (): boolean =>
    (isAnonymousVisitor() || auth.isGuest()) && decoded()?.isOwner !== true;

  const discovery = new DiscoveryClient();

  // Decode + verify the capability in a resource so the panel renders the
  // right state declaratively. Re-runs on navigation.
  const location = useLocation();
  // The fragment is the link secret (sealed-link amendment): re-run on it too.
  const slugSignal = createMemo(() => ({ slug: params.slug, hash: location.hash }));
  /** The ledger answered 429: the link may be fine, so the page says "try
   *  again later" instead of "invalid" (ADR-0028 short-link amendment). */
  const [rateLimited, setRateLimited] = createSignal(false);
  /** The link does not open under its fragment (missing, malformed, wrong
   *  secret, tampered or substituted): docs/ux/call-links.md section 5. */
  const [damaged, setDamaged] = createSignal(false);
  /** The ledger could not be asked for the link (the network, a server
   *  error): the link may be fine, so it is offered again. */
  const [openFailed, setOpenFailed] = createSignal(false);
  const [decoded, { refetch }] = createResource(
    slugSignal,
    async ({ slug, hash }): Promise<DecodedCall | null> => {
      let cap: import("../../wasm/lakko_ffi_wasm").CallInviteCapability | null = null;
      try {
        setRateLimited(false);
        setDamaged(false);
        setOpenFailed(false);
        const wasm = await loadWasm();
        // Sealed-link amendment of ADR-0028 / ADR-0009 (2026-09-28): a
        // missing or malformed fragment is refused before any request, and
        // the blob opens only under the fragment's secret.
        const opened = await openInviteLinkFromUrl({
          slug,
          hash,
          widths: inviteLinkWidths(wasm),
          what: "call-invite",
          fetchPointer: (blobId) => discovery.fetchCallInvite(blobId),
          open: (sealed, secret) => wasm.CallInviteCapability.openLink(sealed, secret),
        });
        if (opened.kind === "not-found") return null;
        cap = opened.capability;
        const identity = chats.identity();
        const isOwner =
          identity !== null &&
          hexKey(identity.signaturePublicKey) ===
            hexKey(cap.ownerSignaturePubkey);
        // The page carries only what the call needs. The link's reach grant
        // is not kept: this web flow joins over the SFU relay and writes
        // nothing to the owner's mailbox.
        return {
          callId: cap.callId,
          ownerAccountId: cap.ownerAccountId,
          ownerSignatureKey: cap.ownerSignaturePubkey,
          ownerFingerprint: cap.ownerFingerprint,
          mediaVideo: cap.mediaVideo,
          valid: cap.isValidNow(BigInt(Date.now())),
          isOwner,
        };
      } catch (err) {
        if (isDiscoveryRateLimited(err)) {
          setRateLimited(true);
          return null;
        }
        if (isInviteLinkDamaged(err)) {
          setDamaged(true);
          return null;
        }
        if (isInviteLinkUnreachable(err)) {
          setOpenFailed(true);
          return null;
        }
        // eslint-disable-next-line no-console
        console.warn("join-call: open/verify failed", err);
        return null;
      } finally {
        cap?.free();
      }
    },
  );

  /** This screen's own state before the host holds the call. */
  const [localPhase, setLocalPhase] = createSignal<"idle" | "starting" | "error">("idle");
  const [error, setError] = createLiveText();
  /** Why the start was refused while another call holds the slot. */
  const [busyMessage, setBusyMessage] = createLiveText();
  // The join to resume once a guest account's session has landed. `null` on
  // the logged-in path.
  const [pendingJoin, setPendingJoin] = createSignal<DecodedCall | null>(null);
  // FINDING 5: synchronous double-tap latch (set BEFORE any await), so a rapid
  // second tap can't mint a SECOND guest account. A second call is prevented by
  // the host, which refuses a call while one holds the slot.
  let mintingGuest = false;
  /** ADR-0054 S4: the guest account pays the signup proof of work. `true`
   *  while it runs; leaving the screen, or `Cancel`, aborts it. */
  const [guestWorking, setGuestWorking] = createSignal(false);
  let guestSignup: AbortController | null = null;
  onCleanup(() => guestSignup?.abort());

  /** The host's link slot, when it is THIS link's call. */
  const hostSlot = createMemo(() => {
    const slot = callHost.linkCall();
    const d = decoded();
    if (slot === null || d === null || d === undefined) return null;
    return slot.callIdHex === callIdHex(d.callId) ? slot : null;
  });

  /** The SFU's room state said `ended` for this link's call when the page
   *  opened: the call is over, so the page says so before anyone presses
   *  Join (`docs/ux/call-links.md` section 5). Any other answer, or none,
   *  changes nothing. */
  const [roomEnded, setRoomEnded] = createSignal(false);
  createEffect(() => {
    const d = decoded();
    setRoomEnded(false);
    if (d === null || d === undefined || !d.valid) return;
    const base = sfuBaseUrl();
    if (base === null) return;
    const callId = d.callId;
    void (async () => {
      try {
        const wasm = await loadWasm();
        const state = await fetchSfuRoomState(base, wasm.sfuRoomForCall(callId));
        // Still the same link?
        if (decoded() !== d) return;
        if (state === "ended") {
          setRoomEnded(true);
          // A join parked behind the guest sign-up has nothing to resume.
          setPendingJoin(null);
        }
      } catch {
        // No answer: the page offers Join as before.
      }
    })();
  });

  const phase = (): Phase =>
    linkCallPagePhase({
      slotPhase: hostSlot()?.phase ?? null,
      localPhase: localPhase(),
      roomEnded: roomEnded(),
    });
  const shownError = (): string | null => hostSlot()?.errorMessage?.() ?? error();

  // While this screen is up it shows the host's state for its call itself, so
  // the host does not repeat it as a notice. An outcome seen here is not
  // repeated on the next screen either.
  const releaseView = callHost.claimLinkCallView();
  onCleanup(() => {
    releaseView();
    const slot = hostSlot();
    if (slot !== null && !linkCallOccupiesSlot(slot.phase)) callHost.dismissLinkCallOutcome();
  });

  // Guest-join resume: once `signupGuest` has landed a full session (state →
  // session, canonical identity present), continue the parked join.
  createEffect(() => {
    if (
      auth.state().kind === "session" &&
      pendingJoin() !== null &&
      chats.identity() !== null &&
      auth.identitySecrets() !== null
    ) {
      const d = pendingJoin();
      setPendingJoin(null);
      if (pendingGuestName !== undefined) {
        const name = pendingGuestName;
        pendingGuestName = undefined;
        // Saving the name never blocks the join.
        void profiles.saveOwnDisplayName(name).catch((err: unknown) => {
          // eslint-disable-next-line no-console
          console.warn("saving the guest profile name failed", err);
        });
      }
      if (d !== null) void beginJoin(d);
    }
  });

  /** Tap Join: use the live session if there is one, else create a guest
   *  account first and go on into the call. */
  async function onStart(d: DecodedCall): Promise<void> {
    setBusyMessage(null);
    const mine = hostSlot();
    if (mine !== null && linkCallOccupiesSlot(mine.phase)) return;
    // Refuse before minting a guest account for a call that could not start
    // (`calls/link-join-gate.ts`): a visitor must not be left holding an
    // account, and be asked whether to keep it, for a call that never began.
    const busy = callHost.callBusyMessage(d.isOwner ? "start" : "join");
    const refusal = linkJoinRefusal({
      canEncryptMedia: isSFrameTransformSupported(),
      busy: busy !== null,
    });
    if (refusal === "no-media-encryption") {
      // Invariant #4: a browser that cannot encrypt call media gets no call.
      // Said here, before the account, the token and the microphone.
      setError(() => t("call.error.no_media_encryption"));
      setLocalPhase("error");
      return;
    }
    if (refusal === "busy") {
      setBusyMessage(busy);
      return;
    }
    if (chats.identity() !== null && auth.identitySecrets() !== null) {
      // A returning guest's name must pass the rule like a new guest's.
      if (auth.isGuest() && !maySaveDisplayName(guestNameCheck())) return;
      await beginJoin(d);
      return;
    }
    // No session — mint a guest account. The resume effect above continues
    // the join once its session lands. The synchronous latch (set BEFORE the
    // await) makes a rapid double-tap idempotent — never two guest accounts.
    if (mintingGuest) return;
    if (!maySaveDisplayName(guestNameCheck())) return;
    pendingGuestName = savedDisplayName(guestNameCheck());
    mintingGuest = true;
    setError(null);
    setLocalPhase("starting");
    setPendingJoin(d);
    const controller = new AbortController();
    guestSignup = controller;
    setGuestWorking(true);
    try {
      await auth.signupGuest({ signal: controller.signal });
    } catch (err) {
      pendingGuestName = undefined;
      setPendingJoin(null);
      if (isAbortError(err)) {
        // Cancelled: back to the Join button, nothing was created.
        setLocalPhase("idle");
        return;
      }
      setError(signupErrorMessage(err));
      setLocalPhase("error");
    } finally {
      if (guestSignup === controller) guestSignup = null;
      setGuestWorking(false);
      // The latch covers the signup itself (a second tap while it runs). Once
      // it is over a tap takes the signed-in branch above; and if that guest
      // account is later deleted on this page (`No, thanks`), Join has to
      // work again (found live 2026-10-02: it did nothing).
      mintingGuest = false;
    }
  }

  function onCancelGuest(): void {
    guestSignup?.abort();
  }

  /** Hand the ADR-0032 host / join to the call host now that a live session
   *  exists. */
  async function beginJoin(d: DecodedCall): Promise<void> {
    setError(null);
    setLocalPhase("starting");
    // A guest's typed name is their name for this call; a person with a
    // real account starts under their profile name and can change it in the
    // call (docs/ux/calls.md section 4.1).
    const callName = auth.isGuest() ? savedDisplayName(guestNameCheck()) : null;
    const outcome = await callHost.startLinkCall({
      ...(callName !== null ? { callName } : {}),
      callId: d.callId,
      ownerAccountId: d.ownerAccountId,
      ownerSignatureKey: d.ownerSignatureKey,
      mediaVideo: d.mediaVideo,
      isOwner: d.isOwner,
      // The fragment is the link secret: without it the page it returns to
      // could not open the link again.
      routePath: `${paths.joinCall(params.slug)}${location.hash}`,
    });
    switch (outcome.kind) {
      case "started":
      case "already-in-this-call":
        // The host's slot is what this screen shows from here on.
        setLocalPhase("idle");
        return;
      case "busy":
        setLocalPhase("idle");
        setBusyMessage(outcome.message);
        return;
      case "failed":
        setError(outcome.message);
        setLocalPhase("error");
        return;
    }
  }

  async function onEndCurrentCall(): Promise<void> {
    await callHost.endCurrentCall();
    setBusyMessage(null);
  }

  // What became of a guest account on this page. Kept: the person goes into
  // the app, where an account belongs and where the guards show the
  // "Protect your account" step (this page would only offer the call again). Deleted: the page
  // says so.
  const navigate = useNavigate();
  let wasGuestHere = false;
  const [guestDeleted, setGuestDeleted] = createSignal(false);
  createEffect(() => {
    if (auth.isGuest()) {
      wasGuestHere = true;
      setGuestDeleted(false);
      return;
    }
    if (!wasGuestHere) return;
    const kind = auth.state().kind;
    if (kind === "anon") {
      wasGuestHere = false;
      setGuestDeleted(true);
      return;
    }
    if (kind === "session" && auth.signupPhrase() === null && !callHost.anyCallInProgress()) {
      wasGuestHere = false;
      navigate(paths.home, { replace: true });
    }
  });

  // A call that ends while this page is open sends a signed-in person back
  // into the app (`docs/ux/call-links.md` section 5). A guest stays: this page
  // is where they are asked about an account.
  createEffect(() => {
    if (
      linkCallPageReturnsToApp({
        phase: phase(),
        signedIn: auth.state().kind === "session",
        isGuest: auth.isGuest(),
      })
    ) {
      navigate(paths.home, { replace: true });
    }
  });

  /** What follows a call that is over: a guest is asked whether to keep the
   *  account; anyone else gets the way into the app. */
  const AfterCall = (): JSX.Element => (
    <Show when={auth.isGuest()} fallback={<A href={paths.home}>{t("common.open_lakko")}</A>}>
      <div class="lk-guest-offer-after">
        <GuestAccountOffer />
      </div>
    </Show>
  );

  function onTryAgain(): void {
    callHost.dismissLinkCallOutcome();
    setError(null);
    setLocalPhase("idle");
  }

  return (
    <Switch
      fallback={
        <PublicLayout>
          <h1>{decoded()?.isOwner ? t("call.link.title_start") : t("call.link.title_join")}</h1>
          <Show when={decoded.loading}>
            <p>{t("call.link.loading")}</p>
          </Show>
          <Show when={!decoded.loading && decoded() === null && rateLimited()}>
            <p>{INVITE_LINK_RATE_LIMITED()}</p>
            <button type="button" class="primary" onClick={() => void refetch()}>
              {INVITE_LINK_TRY_AGAIN()}
            </button>
            <A href={paths.home}>{t("common.open_lakko")}</A>
          </Show>
          <Show when={!decoded.loading && decoded() === null && damaged()}>
            <p>{INVITE_LINK_DAMAGED()}</p>
            <A href={paths.home}>{t("common.open_lakko")}</A>
          </Show>
          <Show when={!decoded.loading && decoded() === null && openFailed()}>
            <p>{INVITE_LINK_OPEN_FAILED()}</p>
            <button type="button" class="primary" onClick={() => void refetch()}>
              {INVITE_LINK_TRY_AGAIN()}
            </button>
            <A href={paths.home}>{t("common.open_lakko")}</A>
          </Show>
          <Show
            when={
              !decoded.loading &&
              ((decoded() === null && !rateLimited() && !damaged() && !openFailed()) ||
                decoded()?.valid === false)
            }
          >
            <p>{t("call.link.invalid")}</p>
            <A href={paths.home}>{t("common.open_lakko")}</A>
          </Show>
          <Show
            when={
              !decoded.loading &&
              decoded() !== null &&
              decoded()?.valid === true
            }
          >
            <Show when={guestDeleted()}>
              <p class="form-info" role="status">
                {t("guest.offer.deleted")}
              </p>
            </Show>
            <Show when={hostName()}>
              {(name) => (
                <p class="lk-person-line">
                  {/* ADR-0084 - the host's photo beside their name. */}
                  <Avatar label={name()} id={hostKeyB64()} size="sm" personKey={hostKeyB64()} />
                  <span>{t("call.link.host", { name: name() })}</span>
                </p>
              )}
            </Show>
            <Show when={busyMessage()}>
              {(message) => (
                <div role="alert">
                  <p>{message()}</p>
                  <button type="button" onClick={() => void onEndCurrentCall()}>
                    {t("call.link.end_current")}
                  </button>
                </div>
              )}
            </Show>
            {/* ADR-0099 step 3: a suspended account joins no call. The name
                field and Join give way to the one line that says so. */}
            <Show when={phase() === "idle" && accountSuspended()}>
              <p class="lk-muted">{t("suspended.self.body")}</p>
              <button type="button" onClick={() => navigate(paths.home, { replace: true })}>
                {t("common.close")}
              </button>
            </Show>
            <Show when={phase() === "idle" && !accountSuspended()}>
              <Show when={asksGuestName()}>
                <label>
                  {t("call.link.guest_name.label")}
                  <input
                    type="text"
                    autocomplete="name"
                    value={guestName()}
                    onInput={(e) => {
                      guestNameEdited = true;
                      setGuestName(e.currentTarget.value);
                    }}
                    aria-invalid={guestNameCheck().kind === "invalid"}
                  />
                </label>
                <Show
                  when={(() => {
                    const c = guestNameCheck();
                    return c.kind === "invalid" ? c.reason : null;
                  })()}
                >
                  {(reason) => (
                    <div class="form-error" role="status" style={{ "font-size": "0.85rem" }}>
                      {reason()}
                    </div>
                  )}
                </Show>
              </Show>
              <button
                type="button"
                class="primary"
                disabled={asksGuestName() && !maySaveDisplayName(guestNameCheck())}
                onClick={() => void onStart(decoded()!)}
              >
                {decoded()?.isOwner ? t("call.link.start") : t("call.link.join")}
              </button>
              {/* No login required: a visitor with no session joins as a
                  guest, and is told so before pressing. */}
              <Show when={isAnonymousVisitor() && !decoded()?.isOwner}>
                <p class="form-info" style={{ margin: 0 }}>
                  {t("call.guest.note")}
                </p>
              </Show>
            </Show>
            <Show when={phase() === "starting" && guestWorking()}>
              {/* docs/ux/anti-abuse.md section 1: the guest account pays the
                  same once-only work as a signup. */}
              <div role="status" aria-live="polite" class="lk-stack" style={{ gap: "8px" }}>
                <button class="primary" type="button" disabled aria-busy="true">
                  <span class="lakko-spinner" aria-hidden="true" /> {guestWorkingTitle()}
                </button>
                <p class="form-info" style={{ margin: 0 }}>
                  {signupWorkingLine()}
                </p>
                <button type="button" onClick={onCancelGuest}>
                  {t("common.cancel")}
                </button>
              </div>
            </Show>
            <Show when={phase() === "starting" && !guestWorking()}>
              <p>{t("call.link.connecting")}</p>
            </Show>
            <Show when={phase() === "waiting"}>
              <p>{t("call.link.waiting")}</p>
              <p style={{ color: "var(--surface-muted)", "font-size": "0.85rem" }}>
                {t("call.link.waiting_hint")}
              </p>
              <button type="button" onClick={() => callHost.leaveLinkCall()}>
                {t("common.cancel")}
              </button>
            </Show>
            <Show when={phase() === "live"}>
              <p>{t("call.link.live")}</p>
            </Show>
            <Show when={phase() === "dead"}>
              <p>{t("call.link.dead")}</p>
              <AfterCall />
            </Show>
            <Show when={phase() === "ended" || phase() === "spent"}>
              <p>{t("call.link.ended")}</p>
              <AfterCall />
            </Show>
            <Show when={phase() === "error"}>
              <p style={{ color: "var(--error)" }}>
                {shownError() === null
                  ? t("call.link.error.generic")
                  : t("call.link.failed", { reason: shownError() ?? "" })}
              </p>
              <button type="button" onClick={onTryAgain}>
                {t("common.try_again")}
              </button>
            </Show>
            {/* A guest who is on this page and not in the call (the join
                failed, they stopped waiting, or the page was reloaded after
                the call) can still be asked the question: this page is the
                only screen a guest has. */}
            <Show when={showsGuestQuestion({ isGuest: auth.isGuest(), phase: phase() })}>
              <div class="lk-guest-offer-after">
                <GuestAccountOffer />
              </div>
            </Show>
          </Show>
        </PublicLayout>
      }
    >
      {/* A guest who chose `Create account`: the recovery phrase, before
          anything else. This route sits outside the guards that draw it for
          every other screen, so it draws it itself. */}
      <Match when={auth.signupPhrase() !== null}>
        <SignupPhrasePrompt />
      </Match>
      {/* A live call is shown by the call host's overlay, on this screen and
          every other one. */}
    </Switch>
  );
}
