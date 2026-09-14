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
 * Once live it presents the shared in-call UI (`<Call embedded>`), driven by
 * the same `callActions` store the orchestrator populates.
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
import { A, useNavigate, useParams } from "@solidjs/router";

import { useAuth } from "../auth/AuthContext";
import { useChats } from "../chat/ChatContext";
import { useProfiles } from "../profile/ProfileContext";
import { createDisplayNameCheck } from "../profile/username-field";
import { maySaveDisplayName, savedDisplayName } from "../profile/rules";
import SaveFreshPhrasePrompt from "../auth/SaveFreshPhrasePrompt";
import { loadWasm } from "../wasm";
import { DiscoveryClient, DiscoveryHttpError } from "../discovery/client";
import { parseCallInviteSlug } from "../calls/invite";
import { sfuBaseUrl } from "../calls/sfu-config";
import { MultiJoinerCall } from "../calls/multi-joiner-call";
import { hexKey } from "../calls/multi-joiner-election";
import Call from "./Call";
import { paths } from "./paths";

/** The landing screen's own state machine. "waiting" is the ADR-0032
 *  joiner-side pre-admission phase (the shared call store stays `connecting`
 *  throughout it); "dead" is the no-live-committer timeout. */
type Phase =
  | "idle"
  | "starting"
  | "waiting"
  | "live"
  | "ended"
  | "dead"
  | "error";

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
  const navigate = useNavigate();

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

  const [phase, setPhase] = createSignal<Phase>("idle");
  const [error, setError] = createSignal<string | null>(null);
  // The join to resume once a guest account's session has landed (after the
  // SaveFreshPhrasePrompt acknowledgement). `null` on the logged-in path.
  const [pendingJoin, setPendingJoin] = createSignal<DecodedCall | null>(null);
  let call: MultiJoinerCall | null = null;
  // FINDING 5: synchronous double-tap latches (set BEFORE any await), so a
  // rapid second tap can't mint a SECOND guest account or start a second call.
  let mintingGuest = false;
  let beginning = false;

  onCleanup(() => {
    call?.hangUp();
    call = null;
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
    setPhase("starting");
    setPendingJoin(d);
    try {
      await auth.signupGuest();
    } catch (err) {
      mintingGuest = false; // allow a retry after a failed mint
      pendingGuestName = undefined;
      setPendingJoin(null);
      setError(err instanceof Error ? err.message : String(err));
      setPhase("error");
    }
  }

  /** Run the actual ADR-0032 host/join now that a live session exists. */
  async function beginJoin(d: DecodedCall): Promise<void> {
    // Synchronous latch (FINDING 5): guard against a rapid double-tap starting
    // two SFU sessions before `call` is assigned past the first `await`.
    if (call !== null || beginning) return;
    beginning = true;
    const identity = chats.identity();
    const secrets = auth.identitySecrets();
    const base = sfuBaseUrl();
    if (identity === null || secrets === null) {
      beginning = false;
      setError("Re-authenticate to join this call (no live session).");
      setPhase("error");
      return;
    }
    if (base === null) {
      beginning = false;
      setError(
        "This client has no SFU endpoint — set VITE_LAKKO_SFU_URL and rebuild.",
      );
      setPhase("error");
      return;
    }
    setError(null);
    setPhase("starting");

    const opts = {
      identity,
      callId: d.callId,
      ownerAccountId: d.ownerAccountId,
      ownerSignatureKey: d.ownerSignatureKey,
      sfuBaseUrl: base,
      sessionToken: secrets.sessionToken,
      media: { audio: true, video: d.mediaVideo },
      onJoined: (): void => {
        setPhase("live");
      },
      onEnded: (): void => {
        setPhase("ended");
      },
      onNoCommitter: (): void => {
        call = null;
        setPhase("dead");
      },
      onError: (err: unknown): void => {
        setError(err instanceof Error ? err.message : String(err));
        setPhase("error");
      },
    };

    try {
      if (d.isOwner) {
        // Owner founds + hosts; `onJoined` fires synchronously inside host().
        call = await MultiJoinerCall.host(opts);
      } else {
        // Joiner connects signaling-only + relays its KeyPackage; the shared
        // store is `connecting` and we sit in "waiting" until the Welcome
        // arrives (onJoined → "live").
        call = await MultiJoinerCall.join(opts);
        if (phase() === "starting") setPhase("waiting");
      }
    } catch (err) {
      beginning = false; // start failed (call unset) — allow a retry
      setError(err instanceof Error ? err.message : String(err));
      setPhase("error");
    }
  }

  function onExit(): void {
    call?.hangUp();
    call = null;
    navigate(paths.home);
  }

  return (
    <Switch
      fallback={
        <article
          style={{ "max-width": "32rem", margin: "2rem auto", padding: "1rem" }}
        >
          <h1>Join call</h1>
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
            <A href={paths.home}>Go home</A>
          </Show>
          <Show
            when={
              !decoded.loading &&
              decoded() !== null &&
              decoded()?.valid === true
            }
          >
            <p>
              Host: <code>{decoded()?.ownerFingerprint}</code>
            </p>
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
            </Show>
            <Show when={phase() === "dead"}>
              <p>This call has ended or isn't active.</p>
              <A href={paths.home}>Go home</A>
            </Show>
            <Show when={phase() === "ended"}>
              <p>The call has ended.</p>
              <A href={paths.home}>Go home</A>
            </Show>
            <Show when={phase() === "error"}>
              <p style={{ color: "var(--error)" }}>Failed: {error()}</p>
            </Show>
          </Show>
        </article>
      }
    >
      {/* Guest onboarding: after signupGuest the auth state is
          `save-fresh-phrase` — surface the existing recovery-phrase prompt.
          On acknowledgement the session lands and the resume effect above
          continues the join. */}
      <Match when={auth.state().kind === "save-fresh-phrase"}>
        <SaveFreshPhrasePrompt />
      </Match>
      <Match when={phase() === "live"}>
        <Call
          embedded
          onExit={onExit}
          remoteStreamFor={(id): MediaStream | undefined =>
            call?.remoteStreamFor(id)
          }
        />
      </Match>
    </Switch>
  );
}
