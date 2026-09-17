// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Owner-side co-publisher management (ADR-0028 OQ3), on the Manage screen:
 * the invite link as a link row with Copy / Share, and pending requests
 * with Approve / Reject.
 *
 * Shared handles come from `ManagePanel` (props); this panel does NOT open
 * or free them — a second handle would fork the MLS epoch. `approveCoPublisher`
 * carries the load-bearing reseal-after-add: the approve commits the Add,
 * the epoch advances, and the new-epoch seed is resealed to every subscriber
 * WITHOUT a second `rotate()`.
 */
import { createSignal, For, onCleanup, Show, type JSX } from "solid-js";

import {
  approveCoPublisher,
  createCoPublisherLink,
  pendingCoPublishers,
  pollCoPublisherJournal,
  rejectCoPublisher,
} from "./copublisher";
import type { WasmIdentity } from "./composer";
import type { PublisherGroupHandle } from "./publisher-group-handle";
// ADR-0045 UX section 6 / 12 item 6 — one refusal table, shared with the chat
// members panel. The co-publisher queue is a different surface but the same
// core refusals, and a second vocabulary for them is the drift 12.6 flags.
import { membershipRefusalOf } from "../chat/membership-refusal";
import { membershipRefusalRetryMaySucceed } from "../../wasm/lakko_ffi_wasm";
import { DsClient } from "../ds/client";
import { channelsActions } from "../state/channels-state";
import { Avatar } from "../ui/Avatar";
import { Icon } from "../ui/icons";

/** Poll cadence for the PublisherGroup DS journal — same 5 s trade-off as
 *  the subscriber fan-out. */
const COPUBLISHER_POLL_INTERVAL_MS = 5_000;

export function CoPublishersPanel(props: {
  readonly channelIdHex: string;
  readonly publisherGroup: PublisherGroupHandle;
  readonly identity: WasmIdentity;
}): JSX.Element {
  const [link, setLink] = createSignal<string | null>(null);
  const [pending, setPending] = createSignal<Uint8Array[]>([]);
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);

  // `ds` is a stateless HTTP client, so a per-panel instance is fine.
  const ds = new DsClient();
  let stopped = false;

  function hexOf(b: Uint8Array): string {
    let s = "";
    for (const x of b) s += x.toString(16).padStart(2, "0");
    return s;
  }

  async function refreshPending(): Promise<void> {
    try {
      await pollCoPublisherJournal(ds, props.identity, props.publisherGroup);
      // An applied co-publisher commit advanced the group in the account
      // identity's provider; the channel log snapshots it on the next flush.
      channelsActions.notePublisherGroupChanged();
      setPending(pendingCoPublishers(props.publisherGroup));
    } catch (err) {
      setError(`Could not check for requests: ${String(err)}`);
    }
  }

  async function onCreateLink(): Promise<void> {
    setError(null);
    setBusy(true);
    try {
      setLink(
        await createCoPublisherLink(props.identity, props.publisherGroup, window.location.origin),
      );
    } catch (err) {
      setError(`Could not create the link: ${String(err)}`);
    } finally {
      setBusy(false);
    }
  }

  async function onApprove(ref: Uint8Array): Promise<void> {
    setError(null);
    setBusy(true);
    try {
      await approveCoPublisher(ds, props.identity, props.publisherGroup, ref, props.channelIdHex);
      // The Add commit advanced the group (and its exporter); snapshot it.
      channelsActions.notePublisherGroupChanged();
      setPending(pendingCoPublishers(props.publisherGroup));
    } catch (err) {
      // ADR-0045 UX section 6 — the co-publisher approve reaches the SAME
      // core refusals as the chat one (`noSuchJoinRequest` when another
      // owner got there first, `joinerAlreadyAMember` when the joiner
      // already landed), so it reads from the same table.
      setError(coPublisherRefusal(err) ?? `Could not approve: ${String(err)}`);
    } finally {
      setBusy(false);
    }
  }

  /** The section-6 sentence for a co-publisher approve / reject refusal,
   *  or `null` when the failure is not a membership refusal (a DS write, a
   *  reseal) and its own text should stand. */
  function coPublisherRefusal(err: unknown): string | null {
    try {
      return membershipRefusalOf(err, membershipRefusalRetryMaySucceed)?.message ?? null;
    } catch {
      return null;
    }
  }

  function onReject(ref: Uint8Array): void {
    setError(null);
    try {
      rejectCoPublisher(props.publisherGroup, ref);
      setPending(pendingCoPublishers(props.publisherGroup));
    } catch (err) {
      setError(coPublisherRefusal(err) ?? `Could not reject: ${String(err)}`);
    }
  }

  // First poll + periodic loop. Cancelled on cleanup.
  void refreshPending();
  const handle = setInterval(() => {
    if (!stopped) void refreshPending();
  }, COPUBLISHER_POLL_INTERVAL_MS);

  onCleanup(() => {
    stopped = true;
    clearInterval(handle);
  });

  const [copied, setCopied] = createSignal(false);
  async function copyLink(): Promise<void> {
    const l = link();
    if (l === null) return;
    try {
      await navigator.clipboard.writeText(l);
      setCopied(true);
      window.setTimeout(() => setCopied(false), 2000);
    } catch {
      /* the button is the only feedback surface */
    }
  }
  const canShare = (): boolean =>
    typeof navigator !== "undefined" && typeof navigator.share === "function";

  return (
    <section class="lk-card">
      <h2>Co-publishers</h2>
      <p class="lk-card-desc">
        People who can post to this channel. Share a link; approve them when they ask.
      </p>
      <Show
        when={link()}
        fallback={
          <div class="lk-actions">
            <button type="button" disabled={busy()} onClick={() => void onCreateLink()}>
              <Icon name="link" size={18} />
              Create invite link
            </button>
          </div>
        }
      >
        {(l) => (
          <div class="lk-link-row" role="group" aria-label="Co-publisher invite link">
            <span class="lk-link-text" title={l()}>
              {l().replace(/^https?:\/\//, "")}
            </span>
            <button
              type="button"
              class="lk-icon-button"
              title={copied() ? "Copied" : "Copy link"}
              aria-label={copied() ? "Copied" : "Copy link"}
              onClick={() => void copyLink()}
            >
              <Icon name={copied() ? "check" : "copy"} size={16} />
            </button>
            <Show when={canShare()}>
              <button
                type="button"
                class="lk-icon-button"
                title="Share"
                aria-label="Share"
                onClick={() => void navigator.share({ url: l() }).catch(() => undefined)}
              >
                <Icon name="share" size={16} />
              </button>
            </Show>
          </div>
        )}
      </Show>
      <Show when={pending().length > 0}>
        <div class="lk-section-label lk-flush">Requests ({pending().length})</div>
        <ul class="lk-list bordered">
          <For each={pending()}>
            {(ref, i) => (
              <li>
                <Avatar label={`R ${i() + 1}`} id={hexOf(ref)} size="sm" />
                <span class="lk-fill">Someone asked to co-publish</span>
                <span class="lk-actions lk-nowrap">
                  <button
                    type="button"
                    class="primary small"
                    disabled={busy()}
                    onClick={() => void onApprove(ref)}
                  >
                    Approve
                  </button>
                  <button type="button" class="small" disabled={busy()} onClick={() => onReject(ref)}>
                    Reject
                  </button>
                </span>
              </li>
            )}
          </For>
        </ul>
      </Show>
      <Show when={error()}>
        {(msg) => (
          <div class="lk-alert danger" role="alert">
            {msg()}
          </div>
        )}
      </Show>
    </section>
  );
}
