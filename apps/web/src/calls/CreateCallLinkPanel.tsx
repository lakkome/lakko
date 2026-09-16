// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * "Create call link" — mints a `/c/` call-invite link (ADR-0032).
 *
 * A call link is independent of any chat, so this lives in the Contacts add
 * menu rather than in a chat header. The owner shares the link and hosts the call from the
 * `/c/<slug>` landing ("Start call" navigates there; `JoinCall.tsx` asks the
 * app-level call host to found and host when the visitor is the capability
 * owner, so the call survives leaving that screen). Anyone holding the link
 * asks to be admitted.
 *
 * The form's decisions (preset lists, label limit, the in-app route) live in
 * `create-call-link.ts`, which is where they are tested.
 */
import { createSignal, For, Show, type JSX } from "solid-js";
import { useNavigate } from "@solidjs/router";

import { useAuth } from "../auth/AuthContext";
import { useChats } from "../chat/ChatContext";
import { Icon } from "../ui/icons";
import { useCallHost } from "./CallHostContext";
import { DiscoveryClient } from "../discovery/client";
import { composeAndUploadCallInvite } from "./composer";
import type { CallMediaPreset, ExpiryPreset, MaxUsesPreset } from "./invite";
import {
  CALL_LINK_DEFAULT_EXPIRY,
  CALL_LINK_DEFAULT_MAX_USES,
  CALL_LINK_EXPIRY_OPTIONS,
  CALL_LINK_MAX_USES_OPTIONS,
  callLinkLabelError,
  callLinkRoutePath,
} from "./create-call-link";

export default function CreateCallLinkPanel(): JSX.Element {
  const auth = useAuth();
  const chats = useChats();
  const navigate = useNavigate();
  const callHost = useCallHost();

  const [media, setMedia] = createSignal<CallMediaPreset>("audio-video");
  const [expiry, setExpiry] = createSignal<ExpiryPreset>(CALL_LINK_DEFAULT_EXPIRY);
  const [maxUses, setMaxUses] = createSignal<MaxUsesPreset>(CALL_LINK_DEFAULT_MAX_USES);
  const [label, setLabel] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  const [link, setLink] = createSignal<string | null>(null);
  const [copied, setCopied] = createSignal(false);

  const labelError = (): string | null => callLinkLabelError(label());

  async function onCreate(e: SubmitEvent): Promise<void> {
    e.preventDefault();
    if (busy() || labelError() !== null) return;
    setError(null);
    setCopied(false);
    // The canonical identity: `JoinCall` recognises the owner by comparing
    // this identity's signature key with the capability's, so a throwaway
    // `Identity.fromSeed` here would mint a link its own creator cannot host.
    const identity = chats.identity();
    const secrets = auth.identitySecrets();
    if (identity === null || secrets === null) {
      setError("Sign in again to create a call link.");
      return;
    }
    setBusy(true);
    try {
      const discovery = new DiscoveryClient();
      discovery.setSessionToken(secrets.sessionToken);
      // Includes a proof-of-work solve (in a worker); the global PoW status
      // line reports progress if it runs long.
      const minted = await composeAndUploadCallInvite({
        ownerSigningSeed: identity.ed25519Seed,
        ownerAccountId: secrets.accountId,
        identity,
        discoveryClient: discovery,
        media: media(),
        expiryPreset: expiry(),
        maxUsesPreset: maxUses(),
        label: label(),
      });
      setLink(minted.url);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  }

  async function onCopy(): Promise<void> {
    const url = link();
    if (url === null) return;
    try {
      await navigator.clipboard.writeText(url);
      setCopied(true);
      window.setTimeout(() => setCopied(false), 2000);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
  }

  function onStart(): void {
    // One call at a time: say so here rather than on the next screen.
    const busy = callHost.callBusyMessage("start");
    if (busy !== null) {
      setError(busy);
      return;
    }
    const url = link();
    const path = url === null ? null : callLinkRoutePath(url);
    if (path === null) {
      setError("This link cannot be opened here.");
      return;
    }
    navigate(path);
  }

  function reset(): void {
    setLink(null);
    setError(null);
    setCopied(false);
    setLabel("");
  }

  return (
    <section class="lk-card">
      <h2>Create a call link</h2>
      <p class="lk-card-desc">
        Anyone with the link can ask to join. You host the call and admit
        people as they arrive. Calls are end-to-end encrypted.
      </p>

      <Show
        when={link() === null}
        fallback={
          <div class="lk-stack" style={{ gap: "10px" }}>
            <div class="lk-link-row" role="group" aria-label="Call link">
              <span class="lk-link-text" title={link() ?? ""}>
                {(link() ?? "").replace(/^https?:\/\//, "")}
              </span>
              <button
                type="button"
                class="lk-icon-button"
                title={copied() ? "Copied" : "Copy link"}
                aria-label={copied() ? "Copied" : "Copy link"}
                onClick={() => void onCopy()}
              >
                <Icon name={copied() ? "check" : "copy"} size={16} />
              </button>
              <Show when={typeof navigator !== "undefined" && typeof navigator.share === "function"}>
                <button
                  type="button"
                  class="lk-icon-button"
                  title="Share"
                  aria-label="Share"
                  onClick={() => void navigator.share({ url: link() ?? "" }).catch(() => undefined)}
                >
                  <Icon name="share" size={16} />
                </button>
              </Show>
            </div>
            {error() !== null && <div class="form-error">{error()}</div>}
            <div class="lk-actions">
              <button class="primary" type="button" onClick={onStart}>
                Start call
              </button>
              <button type="button" onClick={reset}>
                Create another
              </button>
            </div>
          </div>
        }
      >
        <form onSubmit={(e) => void onCreate(e)}>
          <fieldset
            disabled={busy()}
            style={{ border: "none", padding: 0, margin: "0 0 0.6rem 0" }}
          >
            <legend style={{ padding: 0 }}>Call type</legend>
            <div style={{ display: "flex", gap: "1.25rem", "margin-top": "0.25rem" }}>
              <For each={[
                { value: "audio" as const, label: "Audio" },
                { value: "audio-video" as const, label: "Video" },
              ]}>
                {(o) => (
                  <label style={{ display: "flex", gap: "0.4rem", "align-items": "center" }}>
                    {/* The global `input { width: 100% }` rule would stretch a
                        radio across the row. */}
                    <input
                      type="radio"
                      name="call-link-media"
                      value={o.value}
                      checked={media() === o.value}
                      onChange={() => setMedia(o.value)}
                      style={{ width: "auto", margin: 0 }}
                    />
                    <span>{o.label}</span>
                  </label>
                )}
              </For>
            </div>
          </fieldset>

          <label style={{ display: "block", "margin-bottom": "0.6rem" }}>
            <span>Link expires after</span>
            <select
              value={expiry()}
              onChange={(e) => setExpiry(e.currentTarget.value as ExpiryPreset)}
              style={{ width: "100%", "margin-top": "0.25rem" }}
              disabled={busy()}
            >
              <For each={CALL_LINK_EXPIRY_OPTIONS}>
                {(o) => <option value={o.value}>{o.label}</option>}
              </For>
            </select>
          </label>

          <label style={{ display: "block", "margin-bottom": "0.6rem" }}>
            <span>Who can use it</span>
            <select
              value={maxUses()}
              onChange={(e) => setMaxUses(e.currentTarget.value as MaxUsesPreset)}
              style={{ width: "100%", "margin-top": "0.25rem" }}
              disabled={busy()}
            >
              <For each={CALL_LINK_MAX_USES_OPTIONS}>
                {(o) => <option value={o.value}>{o.label}</option>}
              </For>
            </select>
          </label>

          <label style={{ display: "block", "margin-bottom": "0.6rem" }}>
            <span>Label (optional)</span>
            <input
              type="text"
              value={label()}
              onInput={(e) => setLabel(e.currentTarget.value)}
              placeholder="Team standup"
              style={{ width: "100%", "margin-top": "0.25rem" }}
              disabled={busy()}
            />
            <span
              style={{
                color: "var(--surface-muted)",
                "font-size": "0.8rem",
                display: "block",
                "margin-top": "0.2rem",
              }}
            >
              Shown to people who open the link.
            </span>
          </label>

          {labelError() !== null && <div class="form-error">{labelError()}</div>}
          {error() !== null && <div class="form-error">{error()}</div>}

          <button
            class="primary"
            type="submit"
            disabled={busy() || labelError() !== null}
          >
            {busy() ? "Creating link…" : "Create link"}
          </button>
        </form>
      </Show>
    </section>
  );
}
