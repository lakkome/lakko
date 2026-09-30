// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * "Invite links" (docs/ux/client-redesign.md section 5.3): the account's
 * active links, each a row with Copy, Share and Revoke, and a "Create link"
 * form. Never a raw URL in a textarea.
 *
 * The create path is the one that lived in `routes/Contacts.tsx`: compose the
 * capability in the core, upload it to the discovery service for the short
 * pointer URL, fall back to the self-contained inline URL only on a network
 * failure (a rejected upload is a real error; the same bytes would be
 * rejected again by whoever opens them). The list is the owner query the old
 * "Active invites" card made, run when this panel opens and after every
 * change, never behind a button.
 */
import { createSignal, For, onMount, Show, type JSX } from "solid-js";

import { useAuth } from "../auth/AuthContext";
import { base64UrlDecodeNoPad } from "../auth/account-id";
import { useChats } from "../chat/ChatContext";
import { DiscoveryClient, type ActiveInvite } from "../discovery/client";
import { Icon } from "../ui/icons";
import { loadWasm } from "../wasm";
import { joinFacts, t } from "../l10n";
import { KeyedError } from "../l10n/keyed-error";
import { createLiveText } from "../l10n/live-text";
import { contactsErrorText } from "./errors";
import { inviteLinkWidths, type InviteLinkWidths } from "./invite-link";
import {
  composeInlineInviteUrl,
  composePointerInviteUrl,
  expiryPresetToMs,
  INVITE_LABEL_MAX_LEN,
  maxUsesPresetToValue,
  type ExpiryPreset,
  type MaxUsesPreset,
} from "./invite";
import {
  describeInviteCreated,
  describeInviteExpiry,
  describeInviteUses,
} from "./invite-describe";

/** A link this panel can show: an uploaded one from the list, or the inline
 *  fallback minted a moment ago (no server row, so it cannot be revoked). */
interface LinkRow {
  readonly key: string;
  readonly url: string;
  /** The owner's label, or `null` for an unlabelled link (shown as the
   *  catalog's "Invite link"). */
  readonly label: string | null;
  /** The facts under the title, rendered in the current language. */
  readonly meta: () => string;
  readonly revocable: boolean;
  readonly blobIdSlug: string | null;
}

/** `null` when the listed id is not a ledger id of the current width (a row
 *  the server should not have sent); the row is then left out rather than
 *  breaking the list. */
function rowOf(invite: ActiveInvite, nowMs: number, widths: InviteLinkWidths): LinkRow | null {
  let url: string;
  try {
    url = composePointerInviteUrl(base64UrlDecodeNoPad(invite.blobIdSlug), widths);
  } catch {
    return null;
  }
  const meta = (): string => {
    const parts = [
      describeInviteCreated(invite.createdAt),
      describeInviteExpiry(invite.expiryMs, nowMs),
      describeInviteUses(invite.usedCount, invite.maxUses),
    ];
    if (invite.bootstrapReferral) parts.push(t("invite.links.for_new_user"));
    return joinFacts(parts);
  };
  return {
    key: invite.blobIdSlug,
    url,
    label: invite.label.length > 0 ? invite.label : null,
    meta,
    revocable: true,
    blobIdSlug: invite.blobIdSlug,
  };
}

export function InviteLinksPanel(): JSX.Element {
  const auth = useAuth();
  const chats = useChats();
  const discovery = new DiscoveryClient();
  discovery.setSessionToken(auth.identitySecrets()?.sessionToken ?? null);

  const [invites, setInvites] = createSignal<ActiveInvite[]>([]);
  const [widths, setWidths] = createSignal<InviteLinkWidths | null>(null);
  const [loaded, setLoaded] = createSignal(false);
  const [inlineLink, setInlineLink] = createSignal<LinkRow | null>(null);
  const [error, setError] = createLiveText();
  const [notice, setNotice] = createLiveText();
  const [busyKey, setBusyKey] = createSignal<string | null>(null);
  const [copiedKey, setCopiedKey] = createSignal<string | null>(null);

  const [creating, setCreating] = createSignal(false);
  const [label, setLabel] = createSignal("");
  const [expiry, setExpiry] = createSignal<ExpiryPreset>("7d");
  const [maxUses, setMaxUses] = createSignal<MaxUsesPreset>("single");
  const [bootstrapReferral, setBootstrapReferral] = createSignal(false);
  const [createBusy, setCreateBusy] = createSignal(false);

  const rows = (): LinkRow[] => {
    const now = Date.now();
    const w = widths();
    const listed =
      w === null
        ? []
        : invites()
            .map((i) => rowOf(i, now, w))
            .filter((r): r is LinkRow => r !== null);
    const inline = inlineLink();
    return inline === null ? listed : [inline, ...listed];
  };

  onMount(() => {
    void load();
  });

  async function load(): Promise<void> {
    try {
      const secrets = auth.identitySecrets();
      const id = chats.identity();
      if (secrets === null || id === null) {
        throw new KeyedError("invite.links.error.sign_in_list");
      }
      setWidths(inviteLinkWidths(await loadWasm()));
      const result = await discovery.listInvitesByOwner(secrets.accountId, id.ed25519Seed);
      // "not found or not owner" means no row matches this account: no links.
      setInvites(result.kind === "list" ? result.invites : []);
      setLoaded(true);
    } catch (err) {
      setError(contactsErrorText(err, "invite.links.error.load"));
      setLoaded(true);
    }
  }

  async function onCreate(e: SubmitEvent): Promise<void> {
    e.preventDefault();
    setError(null);
    setNotice(null);
    setCreateBusy(true);
    try {
      const id = chats.identity();
      const secrets = auth.identitySecrets();
      if (id === null || secrets === null) {
        throw new KeyedError("invite.links.error.sign_in_create");
      }
      const wasm = await loadWasm();
      const bytes = wasm.InviteCapability.compose(
        id.ed25519Seed,
        secrets.accountId,
        id.x25519PublicKey,
        expiryPresetToMs(expiry(), Date.now()),
        maxUsesPresetToValue(maxUses()),
        bootstrapReferral(),
        label(),
      );
      try {
        await discovery.uploadInvite(bytes);
        setInlineLink(null);
        await load();
      } catch (uploadErr) {
        const isNetwork =
          uploadErr instanceof Error && uploadErr.name === "DiscoveryNetworkError";
        if (!isNetwork) throw uploadErr;
        // eslint-disable-next-line no-console
        console.warn("invite upload network-failed; falling back to inline-bytes URL", uploadErr);
        setInlineLink({
          key: "inline",
          url: composeInlineInviteUrl(bytes),
          label: label().length > 0 ? label() : null,
          meta: () => t("invite.links.inline_note"),
          revocable: false,
          blobIdSlug: null,
        });
      }
      setCreating(false);
      setLabel("");
      setBootstrapReferral(false);
    } catch (err) {
      setError(contactsErrorText(err, "chat.error.create_link"));
    } finally {
      setCreateBusy(false);
    }
  }

  async function onCopy(row: LinkRow): Promise<void> {
    try {
      await navigator.clipboard.writeText(row.url);
      setCopiedKey(row.key);
      window.setTimeout(() => setCopiedKey((k) => (k === row.key ? null : k)), 2000);
    } catch (err) {
      console.warn("[contacts] copy failed", err);
      setError(() => t("invite.links.error.copy"));
    }
  }

  async function onShare(row: LinkRow): Promise<void> {
    if (typeof navigator !== "undefined" && typeof navigator.share === "function") {
      try {
        await navigator.share({ url: row.url });
        return;
      } catch {
        // Cancelled or unsupported for this payload: fall through to Copy.
      }
    }
    await onCopy(row);
  }

  async function onRevoke(row: LinkRow): Promise<void> {
    if (row.blobIdSlug === null) return;
    setError(null);
    setNotice(null);
    setBusyKey(row.key);
    try {
      const id = chats.identity();
      if (id === null) throw new KeyedError("invite.links.error.sign_in_revoke");
      const result = await discovery.revokeInvite(base64UrlDecodeNoPad(row.blobIdSlug), id.ed25519Seed);
      // Both outcomes mean the row is gone server-side.
      setInvites(invites().filter((i) => i.blobIdSlug !== row.blobIdSlug));
      const label = row.label;
      setNotice(() =>
        result.kind === "revoked"
          ? t("invite.links.revoked")
          : label === null
            ? t("invite.links.already_removed_unnamed")
            : t("invite.links.already_removed", { label }),
      );
    } catch (err) {
      setError(contactsErrorText(err, "chat.error.revoke_link"));
    } finally {
      setBusyKey(null);
    }
  }

  return (
    <section class="lk-card">
      <h2>{t("contacts.invite_links.title")}</h2>
      <p class="lk-card-desc">{t("invite.links.desc")}</p>

      <Show when={error()}>{(m) => <div class="form-error">{m()}</div>}</Show>
      <Show when={notice()}>{(m) => <div class="form-info">{m()}</div>}</Show>

      <Show
        when={creating()}
        fallback={
          <div class="lk-actions">
            <button type="button" class="primary" onClick={() => setCreating(true)}>
              <Icon name="plus" size={18} />
              {t("invite.links.create")}
            </button>
          </div>
        }
      >
        <form onSubmit={(e) => void onCreate(e)} class="lk-stack" style={{ gap: "12px" }}>
          <label>
            {t("invite.links.label_field")}
            <input
              type="text"
              value={label()}
              onInput={(e) => setLabel(e.currentTarget.value)}
              placeholder={t("invite.links.label_placeholder")}
              maxlength={INVITE_LABEL_MAX_LEN}
              disabled={createBusy()}
            />
          </label>
          <label>
            {t("invite.links.expires_field")}
            <select
              value={expiry()}
              onChange={(e) => setExpiry(e.currentTarget.value as ExpiryPreset)}
              disabled={createBusy()}
            >
              <option value="1d">{t("invite.links.expires_option_days", { count: 1 })}</option>
              <option value="7d">{t("invite.links.expires_option_days", { count: 7 })}</option>
              <option value="30d">{t("invite.links.expires_option_days", { count: 30 })}</option>
              <option value="never">{t("invite.links.expires_option_never")}</option>
            </select>
          </label>
          <label>
            {t("invite.links.uses_field")}
            <select
              value={maxUses()}
              onChange={(e) => setMaxUses(e.currentTarget.value as MaxUsesPreset)}
              disabled={createBusy()}
            >
              <option value="single">{t("invite.links.uses_option_one")}</option>
              <option value="ten">{t("invite.links.uses_option_up_to", { count: 10 })}</option>
              <option value="unlimited">{t("invite.links.uses_option_unlimited")}</option>
            </select>
          </label>
          <label style={{ display: "flex", gap: "8px", "align-items": "center" }}>
            <input
              type="checkbox"
              checked={bootstrapReferral()}
              onChange={(e) => setBootstrapReferral(e.currentTarget.checked)}
              disabled={createBusy()}
              style={{ width: "auto", margin: 0 }}
            />
            <span>{t("invite.links.referral_option")}</span>
          </label>
          <div class="lk-actions">
            <button class="primary" type="submit" disabled={createBusy()}>
              {createBusy() ? t("chats.new_group.creating") : t("invite.links.create")}
            </button>
            <button type="button" class="ghost" disabled={createBusy()} onClick={() => setCreating(false)}>
              {t("common.cancel")}
            </button>
          </div>
        </form>
      </Show>

      <Show when={loaded()} fallback={<p class="lk-muted">{t("common.loading")}</p>}>
        <Show when={rows().length > 0} fallback={<p class="lk-muted">{t("invite.links.empty")}</p>}>
          <ul class="lk-list lk-invite-list" aria-label={t("invite.links.list")}>
            <For each={rows()}>
              {(row) => (
                <li class="lk-invite-item">
                  <div class="lk-invite-title">
                    <strong>{row.label ?? t("chat.invite_link.label")}</strong>
                    <span class="lk-meta">{row.meta()}</span>
                  </div>
                  <div class="lk-link-row" role="group" aria-label={row.label ?? t("chat.invite_link.label")}>
                    <span class="lk-link-text" title={row.url}>
                      {row.url.replace(/^https?:\/\//, "")}
                    </span>
                    <button
                      type="button"
                      class="lk-icon-button"
                      title={copiedKey() === row.key ? t("common.copied") : t("chat.invite_link.copy")}
                      aria-label={copiedKey() === row.key ? t("common.copied") : t("chat.invite_link.copy")}
                      onClick={() => void onCopy(row)}
                    >
                      <Icon name={copiedKey() === row.key ? "check" : "copy"} size={16} />
                    </button>
                    <button
                      type="button"
                      class="lk-icon-button"
                      title={t("chat.invite_link.share")}
                      aria-label={t("chat.invite_link.share")}
                      onClick={() => void onShare(row)}
                    >
                      <Icon name="share" size={16} />
                    </button>
                    <Show when={row.revocable}>
                      <button
                        type="button"
                        class="ghost small"
                        style={{ color: "var(--danger)" }}
                        disabled={busyKey() !== null}
                        onClick={() => void onRevoke(row)}
                      >
                        {busyKey() === row.key ? t("invite.links.revoking") : t("chat.invite_link.revoke")}
                      </button>
                    </Show>
                  </div>
                </li>
              )}
            </For>
          </ul>
        </Show>
      </Show>
    </section>
  );
}
