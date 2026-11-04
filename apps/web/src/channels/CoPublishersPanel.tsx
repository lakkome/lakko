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
import { DiscoveryClient } from "../discovery/client";
import { useAuth } from "../auth/AuthContext";
import { base64Encode } from "../auth/account-id";
import { channelsActions } from "../state/channels-state";
import { t } from "../l10n";
import { createLiveText } from "../l10n/live-text";
import { Avatar } from "../ui/Avatar";
import { Icon } from "../ui/icons";
import { publicOrigin } from "../platform";

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
  const [error, setError] = createLiveText();

  // `ds` is a stateless HTTP client, so a per-panel instance is fine.
  const ds = new DsClient();
  const auth = useAuth();
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
      console.warn("[channels] co-publisher poll failed", err);
      setError(() => t("channels.copublishers.error.poll"));
    }
  }

  async function onCreateLink(): Promise<void> {
    setError(null);
    setBusy(true);
    try {
      // The link is uploaded to the group-invite ledger (no inline form since
      // 2026-10-19): any failed upload, a network one included, mints nothing
      // and shows the error; the button stays for another try.
      const secrets = auth.identitySecrets();
      const discovery = new DiscoveryClient();
      discovery.setSessionToken(secrets?.sessionToken ?? null);
      setLink(
        await createCoPublisherLink(
          props.identity,
          props.publisherGroup,
          publicOrigin(),
          discovery,
          secrets === null ? null : base64Encode(secrets.accountId),
        ),
      );
    } catch (err) {
      console.warn("[channels] co-publisher link failed", err);
      setError(() => t("chat.error.create_link"));
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
      if (coPublisherRefusal(err) === null) console.warn("[channels] approve failed", err);
      setError(() => coPublisherRefusal(err) ?? t("channels.copublishers.error.approve"));
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
      rejectCoPublisher(props.identity, props.publisherGroup, ref);
      // The retained set lives in the provider's MLS storage (ADR-0027
      // update note); snapshot it so the rejection survives a restart, the
      // same flush the approve schedules.
      channelsActions.notePublisherGroupChanged();
      setPending(pendingCoPublishers(props.publisherGroup));
    } catch (err) {
      if (coPublisherRefusal(err) === null) console.warn("[channels] reject failed", err);
      setError(() => coPublisherRefusal(err) ?? t("channels.copublishers.error.reject"));
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
  const [copyFailed, setCopyFailed] = createSignal(false);
  async function copyLink(): Promise<void> {
    const l = link();
    if (l === null) return;
    setCopyFailed(false);
    try {
      await navigator.clipboard.writeText(l);
      setCopied(true);
      window.setTimeout(() => setCopied(false), 2000);
    } catch {
      setCopyFailed(true);
    }
  }
  return (
    <section class="lk-card">
      <h2>{t("channels.copublishers.title")}</h2>
      <p class="lk-card-desc">{t("channels.copublishers.desc")}</p>
      <Show
        when={link()}
        fallback={
          <div class="lk-actions">
            <button type="button" disabled={busy()} onClick={() => void onCreateLink()}>
              <Icon name="link" size={18} />
              {t("chat.invite_link.create")}
            </button>
          </div>
        }
      >
        {(l) => (
          <div class="lk-link-row" role="group" aria-label={t("channels.copublishers.link")}>
            <span class="lk-link-text" title={l()}>
              {l().replace(/^https?:\/\//, "")}
            </span>
            <button
              type="button"
              class="lk-icon-button"
              title={copied() ? t("common.copied") : t("chat.invite_link.copy")}
              aria-label={copied() ? t("common.copied") : t("chat.invite_link.copy")}
              onClick={() => void copyLink()}
            >
              <Icon name={copied() ? "check" : "copy"} size={16} />
            </button>
          </div>
        )}
      </Show>
      <Show when={copied()}>
        <p class="form-info" role="status">
          {t("chat.invite_link.copied")}
        </p>
      </Show>
      <Show when={copyFailed()}>
        <p class="form-error" role="alert">
          {t("chat.invite_link.copy_failed")}
        </p>
      </Show>
      <Show when={pending().length > 0}>
        <div class="lk-section-label lk-flush">
          {t("channels.copublishers.requests", { count: pending().length })}
        </div>
        <ul class="lk-list bordered">
          <For each={pending()}>
            {(ref, i) => (
              <li>
                <Avatar label={`R ${i() + 1}`} id={hexOf(ref)} size="sm" />
                <span class="lk-fill">{t("channels.copublishers.request_row")}</span>
                <span class="lk-actions lk-nowrap">
                  <button
                    type="button"
                    class="primary small"
                    disabled={busy()}
                    onClick={() => void onApprove(ref)}
                  >
                    {t("chat.members.join.approve")}
                  </button>
                  <button type="button" class="small" disabled={busy()} onClick={() => onReject(ref)}>
                    {t("chat.members.join.reject")}
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
