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
 * ## Guest join (no login required, ADR-0032 UX)
 *
 * An anonymous visitor (no session) can join WITHOUT logging in: on tapping
 * Join we auto-create a FULL, username-less account via `auth.signupGuest()`
 * (reusing the audited signup machinery — see AuthContext). That flips to the
 * `save-fresh-phrase` state, so we render the existing `SaveFreshPhrasePrompt`
 * for the guest to save their recovery phrase; once they acknowledge,
 * `confirmFreshPhraseSaved` lands the session and the join resumes
 * automatically. Claiming a username is a later onboarding step (the account
 * is full from the start, just username-less).
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
import {
  Match,
  Show,
  Switch,
  createEffect,
  createMemo,
  createResource,
  createSignal,
  onCleanup,
} from "solid-js";
import { A, useParams } from "@solidjs/router";

import { useAuth } from "../auth/AuthContext";
import { useChats } from "../chat/ChatContext";
import { useProfiles } from "../profile/ProfileContext";
import { createDisplayNameCheck } from "../profile/username-field";
import { maySaveDisplayName, savedDisplayName } from "../profile/rules";
import SaveFreshPhrasePrompt from "../auth/SaveFreshPhrasePrompt";
import { loadWasm } from "../wasm";
import { DiscoveryClient, DiscoveryHttpError } from "../discovery/client";
import { parseCallInviteSlug } from "../calls/invite";
import { useCallHost, type CallHostApi } from "../calls/CallHostContext";
import { callIdHex } from "../calls/call-signaling-dispatch";
import { hexKey } from "../calls/multi-joiner-election";
import { linkCallOccupiesSlot, type LinkCallPhase } from "../calls/link-call-host";
import { paths } from "./paths";
import { PublicLayout } from "../ui/PublicLayout";

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
  const callHost: CallHostApi = useCallHost();

  // ADR-0048 UX §4 — a guest (no account yet) is asked their name here,
  // prefilled `Guest`, and it becomes the new account's profile name.
  const [guestName, setGuestName] = createSignal("Guest");
  const guestNameCheck = createDisplayNameCheck(guestName);
  /** The name to save once the guest session lands; `undefined` = nothing to
   *  save (the visitor already had a session). */
  let pendingGuestName: string | null | undefined;
  const isAnonymousVisitor = (): boolean =>
    chats.identity() === null || auth.identitySecrets() === null;

  const discovery = new DiscoveryClient();

  // Decode + verify the capability in a resource so the panel renders the
  // right state declaratively. Re-runs on navigation.
  const slugSignal = createMemo(() => params.slug);
  const [decoded] = createResource(
    slugSignal,
    async (slug): Promise<DecodedCall | null> => {
      try {
        const parsed = parseCallInviteSlug(slug);
        let capabilityBytes: Uint8Array;
        if (parsed.kind === "pointer") {
          const fetched = await discovery.fetchCallInvite(parsed.blobId);
          if (fetched === null) return null;
          capabilityBytes = fetched;
        } else {
          capabilityBytes = parsed.capabilityBytes;
        }
        const wasm = await loadWasm();
        const cap = wasm.CallInviteCapability.decodeAndVerify(capabilityBytes);
        const identity = chats.identity();
        const isOwner =
          identity !== null &&
          hexKey(identity.signaturePublicKey) ===
            hexKey(cap.ownerSignaturePubkey);
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
        if (err instanceof DiscoveryHttpError && err.status === 404) return null;
        // eslint-disable-next-line no-console
        console.warn("join-call: decode/verify failed", err);
        return null;
      }
    },
  );

  /** This screen's own state before the host holds the call. */
  const [localPhase, setLocalPhase] = createSignal<"idle" | "starting" | "error">("idle");
  const [error, setError] = createSignal<string | null>(null);
  /** Why the start was refused while another call holds the slot. */
  const [busyMessage, setBusyMessage] = createSignal<string | null>(null);
  // The join to resume once a guest account's session has landed (after the
  // SaveFreshPhrasePrompt acknowledgement). `null` on the logged-in path.
  const [pendingJoin, setPendingJoin] = createSignal<DecodedCall | null>(null);
  // FINDING 5: synchronous double-tap latch (set BEFORE any await), so a rapid
  // second tap can't mint a SECOND guest account. A second call is prevented by
  // the host, which refuses a call while one holds the slot.
  let mintingGuest = false;

  /** The host's link slot, when it is THIS link's call. */
  const hostSlot = createMemo(() => {
    const slot = callHost.linkCall();
    const d = decoded();
    if (slot === null || d === null || d === undefined) return null;
    return slot.callIdHex === callIdHex(d.callId) ? slot : null;
  });

  const phase = (): Phase => {
    const slot = hostSlot();
    if (slot !== null) return slot.phase;
    return localPhase();
  };
  const shownError = (): string | null => hostSlot()?.errorMessage ?? error();

  // While this screen is up it shows the host's state for its call itself, so
  // the host does not repeat it as a notice. An outcome seen here is not
  // repeated on the next screen either.
  const releaseView = callHost.claimLinkCallView();
  onCleanup(() => {
    releaseView();
    const slot = hostSlot();
    if (slot !== null && !linkCallOccupiesSlot(slot.phase)) callHost.dismissLinkCallOutcome();
  });

  // Guest-join resume: once `signupGuest` + the SaveFreshPhrasePrompt land a
  // full session (state → session, canonical identity present), continue the
  // parked join automatically.
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
   *  account first (username-less full account) and resume after the phrase
   *  is saved. */
  async function onStart(d: DecodedCall): Promise<void> {
    setBusyMessage(null);
    const mine = hostSlot();
    if (mine !== null && linkCallOccupiesSlot(mine.phase)) return;
    // Refuse before minting a guest account for a call that could not start.
    const busy = callHost.callBusyMessage(d.isOwner ? "start" : "join");
    if (busy !== null) {
      setBusyMessage(busy);
      return;
    }
    if (chats.identity() !== null && auth.identitySecrets() !== null) {
      await beginJoin(d);
      return;
    }
    // No session — mint a guest account. This flips auth to `save-fresh-phrase`
    // (the SaveFreshPhrasePrompt renders); the resume effect above continues
    // the join once the session lands. The synchronous latch (set BEFORE the
    // await) makes a rapid double-tap idempotent — never two guest accounts.
    if (mintingGuest) return;
    if (!maySaveDisplayName(guestNameCheck())) return;
    pendingGuestName = savedDisplayName(guestNameCheck());
    mintingGuest = true;
    setError(null);
    setLocalPhase("starting");
    setPendingJoin(d);
    try {
      await auth.signupGuest();
    } catch (err) {
      mintingGuest = false; // allow a retry after a failed mint
      pendingGuestName = undefined;
      setPendingJoin(null);
      setError(err instanceof Error ? err.message : String(err));
      setLocalPhase("error");
    }
  }

  /** Hand the ADR-0032 host / join to the call host now that a live session
   *  exists. */
  async function beginJoin(d: DecodedCall): Promise<void> {
    setError(null);
    setLocalPhase("starting");
    const outcome = await callHost.startLinkCall({
      callId: d.callId,
      ownerAccountId: d.ownerAccountId,
      ownerSignatureKey: d.ownerSignatureKey,
      mediaVideo: d.mediaVideo,
      isOwner: d.isOwner,
      routePath: paths.joinCall(params.slug),
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

  function onTryAgain(): void {
    callHost.dismissLinkCallOutcome();
    setError(null);
    setLocalPhase("idle");
  }

  return (
    <Switch
      fallback={
        <PublicLayout>
          <h1>{decoded()?.isOwner ? "Start your call" : "Join a call"}</h1>
          <Show when={decoded.loading}>
            <p>Loading call link…</p>
          </Show>
          <Show
            when={
              !decoded.loading &&
              (decoded() === null || decoded()?.valid === false)
            }
          >
            <p>This call link is invalid or has expired.</p>
            <A href={paths.home}>Open Lakko</A>
          </Show>
          <Show
            when={
              !decoded.loading &&
              decoded() !== null &&
              decoded()?.valid === true
            }
          >
            <Show when={hostName()}>
              {(name) => <p>Host: {name()}</p>}
            </Show>
            <Show when={busyMessage()}>
              {(message) => (
                <div role="alert">
                  <p>{message()}</p>
                  <button type="button" onClick={() => void onEndCurrentCall()}>
                    End current call
                  </button>
                </div>
              )}
            </Show>
            <Show when={phase() === "idle"}>
              <Show when={isAnonymousVisitor() && !decoded()?.isOwner}>
                <label>
                  Your name
                  <input
                    type="text"
                    autocomplete="name"
                    value={guestName()}
                    onInput={(e) => setGuestName(e.currentTarget.value)}
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
                disabled={isAnonymousVisitor() && !maySaveDisplayName(guestNameCheck())}
                onClick={() => void onStart(decoded()!)}
              >
                {decoded()?.isOwner ? "Start call" : "Join call"}
              </button>
              {/* No login required: a joiner with no session gets a full,
                  username-less guest account (auth.signupGuest) on tap. */}
            </Show>
            <Show when={phase() === "starting"}>
              <p>Connecting…</p>
            </Show>
            <Show when={phase() === "waiting"}>
              <p>Waiting for the owner to admit you…</p>
              <p style={{ color: "var(--surface-muted)", "font-size": "0.85rem" }}>
                You can leave this screen; you will join when you are admitted.
              </p>
              <button type="button" onClick={() => callHost.leaveLinkCall()}>
                Cancel
              </button>
            </Show>
            <Show when={phase() === "live"}>
              <p>You are in this call.</p>
            </Show>
            <Show when={phase() === "dead"}>
              <p>This call has ended or is not active.</p>
              <A href={paths.home}>Open Lakko</A>
            </Show>
            <Show when={phase() === "ended"}>
              <p>The call has ended.</p>
              <A href={paths.home}>Open Lakko</A>
            </Show>
            <Show when={phase() === "error"}>
              <p style={{ color: "var(--error)" }}>Failed: {shownError()}</p>
              <button type="button" onClick={onTryAgain}>
                Try again
              </button>
            </Show>
          </Show>
        </PublicLayout>
      }
    >
      {/* Guest onboarding: after signupGuest the auth state is
          `save-fresh-phrase` — surface the existing recovery-phrase prompt.
          On acknowledgement the session lands and the resume effect above
          continues the join. */}
      <Match when={auth.state().kind === "save-fresh-phrase"}>
        <SaveFreshPhrasePrompt />
      </Match>
      {/* A live call is shown by the call host's overlay, on this screen and
          every other one. */}
    </Switch>
  );
}
