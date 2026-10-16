// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * "Invite links" (docs/ux/client-redesign.md section 5.3): the account's
 * active links, each a row with Copy, Share and Revoke, and a "Create link"
 * form. Never a raw URL in a textarea.
 *
 * The create path is the one that lived in `routes/Contacts.tsx`: register
 * the link's reach key (ADR-0060 section 6) before anything else, compose and
 * seal the capability in the core, upload the sealed blob to the discovery
 * service for the short pointer URL (the secret stays in the fragment and in
 * this device's record of the link, `minted-links.ts`, beside the link's
 * reach scope id and its own revocation seed), fall back to the
 * self-contained inline URL only on a network failure (a rejected upload is a
 * real error; the same bytes would be rejected again by whoever opens them).
 *
 * ADR-0078 section 10: the ledger rows name no owner, so there is no owner
 * query. The list is this device's own records of the links it minted, read
 * when this panel opens and after every change; a link minted on another
 * device is listed and revoked there. Revoking signs with the link's own
 * revocation seed, never the account key.
 */
import { createSignal, For, onMount, Show, type JSX } from "solid-js";

import { useAuth } from "../auth/AuthContext";
import { base64Encode, base64UrlDecodeNoPad, base64UrlEncodeNoPad } from "../auth/account-id";
import { useChats } from "../chat/ChatContext";
import { DiscoveryClient } from "../discovery/client";
import { Icon } from "../ui/icons";
import { loadWasm } from "../wasm";
import { joinFacts, t } from "../l10n";
import { KeyedError } from "../l10n/keyed-error";
import { createLiveText } from "../l10n/live-text";
import { contactsErrorText } from "./errors";
import { reachSession } from "../reach/reach-service";
import {
  isInlineLinkSlug,
  listMintedLinks,
  rememberMintedLink,
  type MintedLinkFacts,
  type MintedLinkRow,
} from "./minted-links";
import { newRevocationKey, revokeMintedLink } from "./own-links";
import {
  appendInviteLinkSecret,
  composeSealedInviteLink,
  inviteLinkWidths,
  type InviteLinkWidths,
} from "./invite-link";
import { forgetInviteLinkSecret } from "./invite-link-secrets";
import {
  composePointerInviteUrl,
  INVITE_URL_PREFIX,
  expiryPresetToMs,
  INVITE_LABEL_MAX_LEN,
  maxUsesPresetToValue,
  type ExpiryPreset,
  type MaxUsesPreset,
} from "./invite";
import { describeInviteCreated, describeInviteExpiry, describeInviteLimit } from "./invite-describe";

/** A link this panel can show: one this device minted and keeps a record
 *  of, or the inline fallback minted a moment ago (no server row, so it
 *  cannot be revoked). */
interface LinkRow {
  readonly key: string;
  readonly url: string | null;
  /** The owner's label, or `null` for an unlabelled link (shown as the
   *  catalog's "Invite link"). */
  readonly label: string | null;
  /** The facts under the title, rendered in the current language. */
  readonly meta: () => string;
  readonly revocable: boolean;
  /** The record the revoke acts on; `null` for the inline fallback. */
  readonly link: MintedLinkRow | null;
}

/** A row for one kept record, or `null` for a record of another shape (a
 *  ledger id of another width, no facts kept). */
function rowOf(link: MintedLinkRow, nowMs: number, widths: InviteLinkWidths): LinkRow | null {
  const facts: MintedLinkFacts | null = link.facts;
  if (facts === null || isInlineLinkSlug(link.blobIdSlug)) return null;
  let url: string;
  try {
    const pointer = composePointerInviteUrl(base64UrlDecodeNoPad(link.blobIdSlug), widths);
    url = `${pointer}${appendInviteLinkSecret("", link.linkSecret, widths)}`;
  } catch {
    return null;
  }
  const meta = (): string => {
    const parts = [
      link.mintedAtMs === null ? "" : describeInviteCreated(new Date(link.mintedAtMs).toISOString()),
      describeInviteExpiry(facts.expiryMs, nowMs),
      describeInviteLimit(facts.maxUses),
    ].filter((p) => p.length > 0);
    if (facts.bootstrapReferral) parts.push(t("invite.links.for_new_user"));
    return joinFacts(parts);
  };
  return {
    key: link.blobIdSlug,
    url,
    label: facts.label.length > 0 ? facts.label : null,
    meta,
    revocable: link.revocationSeed !== null,
    link,
  };
}

export function InviteLinksPanel(): JSX.Element {
  const auth = useAuth();
  const chats = useChats();
  const discovery = new DiscoveryClient();
  discovery.setSessionToken(auth.identitySecrets()?.sessionToken ?? null);

  const [links, setLinks] = createSignal<MintedLinkRow[]>([]);
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
        : links()
            .map((l) => rowOf(l, now, w))
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
      if (secrets === null) {
        throw new KeyedError("invite.links.error.sign_in_list");
      }
      setWidths(inviteLinkWidths(await loadWasm()));
      // ADR-0078 section 10: this device's records, newest first. An expired
      // link is left out (the ledger drops it on its own).
      const now = Date.now();
      const own = (await listMintedLinks(base64Encode(secrets.accountId)))
        .filter((l) => l.kind === "contact")
        .filter((l) => l.facts === null || l.facts.expiryMs === 0 || l.facts.expiryMs > now)
        .sort((x, y) => (y.mintedAtMs ?? 0) - (x.mintedAtMs ?? 0));
      setLinks(own);
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
      // The account key signs the capability, never this device's key: the
      // link grant carries the account's mailbox binding and the core refuses
      // a grant not bound to the capability's signer (ADR-0078). iOS and
      // Android sign with the account seed as well.
      const id = chats.accountIdentity();
      const secrets = auth.identitySecrets();
      if (id === null || secrets === null) {
        throw new KeyedError("invite.links.error.sign_in_create");
      }
      // ADR-0060 section 6: the link's reach key is registered before the
      // link exists; a link shared before its key is registered does not
      // work. No reach session, or a failed registration, fails the mint.
      const session = reachSession();
      if (session === null) {
        throw new KeyedError("common.error.starting_up");
      }
      const wasm = await loadWasm();
      const widths = inviteLinkWidths(wasm);
      const expiryMs = expiryPresetToMs(expiry(), Date.now());
      const maxUsesValue = maxUsesPresetToValue(maxUses());
      const linkId = wasm.generateLinkId();
      const linkGrant = await session.newLink(linkId, expiryMs === 0n ? null : Number(expiryMs));
      // Sealed-link amendment of ADR-0028 (2026-09-28): the compose seals the
      // capability under a fresh link secret; the ledger gets the sealed blob
      // and the clear fields, the secret rides only in the link's fragment.
      let mint: import("../../wasm/lakko_ffi_wasm").InviteLinkMint;
      try {
        mint = wasm.InviteCapability.compose(
          id.ed25519Seed,
          secrets.accountId,
          id.x25519PublicKey,
          expiryMs,
          maxUsesValue,
          bootstrapReferral(),
          label(),
          linkId,
          linkGrant,
        );
      } catch (composeErr) {
        await session.revokeLink(linkId).catch(() => undefined);
        throw composeErr;
      } finally {
        linkGrant.free();
      }
      const sealed = mint.sealed;
      const linkSecret = mint.linkSecret;
      mint.free();
      // ADR-0078 section 10: the ledger row carries a key minted for this
      // link alone, never the account key.
      const revocation = await newRevocationKey();
      try {
        const blobId = await discovery.uploadInvite({
          sealed,
          revocationPubkey: revocation.publicKey,
          expiryMs,
          maxUses: maxUsesValue,
        });
        // The record is the only list of this account's links and the only
        // key that revokes this one: the secret (to show the working link),
        // the link id (ADR-0060 section 6: removes its reach key), the
        // revocation seed, and what the list shows.
        await rememberMintedLink(
          base64Encode(secrets.accountId),
          "contact",
          base64UrlEncodeNoPad(blobId),
          {
            linkSecret,
            linkId,
            revocationSeed: revocation.seed,
            facts: {
              label: label(),
              expiryMs: Number(expiryMs),
              maxUses: maxUsesValue,
              bootstrapReferral: bootstrapReferral(),
            },
          },
        );
        setInlineLink(null);
        await load();
      } catch (uploadErr) {
        const isNetwork =
          uploadErr instanceof Error && uploadErr.name === "DiscoveryNetworkError";
        if (!isNetwork) {
          // Nothing was handed out: the key goes with the failed link.
          await session.revokeLink(linkId).catch(() => undefined);
          throw uploadErr;
        }
        // eslint-disable-next-line no-console
        console.warn("invite upload network-failed; falling back to the inline link");
        setInlineLink({
          key: "inline",
          url: composeSealedInviteLink({
            origin: location.origin,
            prefix: INVITE_URL_PREFIX,
            blobId: null,
            sealed,
            secret: linkSecret,
            widths,
          }),
          label: label().length > 0 ? label() : null,
          meta: () => t("invite.links.inline_note"),
          revocable: false,
          link: null,
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
    if (row.url === null) return;
    try {
      await navigator.clipboard.writeText(row.url);
      setCopiedKey(row.key);
      window.setTimeout(() => setCopiedKey((k) => (k === row.key ? null : k)), 2000);
    } catch (err) {
      console.warn("[contacts] copy failed", err);
      setError(() => t("invite.links.error.copy"));
    }
  }

  async function onRevoke(row: LinkRow): Promise<void> {
    const link = row.link;
    if (link === null) return;
    setError(null);
    setNotice(null);
    setBusyKey(row.key);
    try {
      const secrets = auth.identitySecrets();
      if (secrets === null) throw new KeyedError("invite.links.error.sign_in_revoke");
      // ADR-0078 section 10: signed by the link's own revocation seed; the
      // link's reach key (ADR-0060 section 6) and the record go with it, so a
      // request written under the link is dropped even by a joiner who
      // fetched it.
      const outcome = await revokeMintedLink(discovery, base64Encode(secrets.accountId), link);
      if (outcome === "unreachable") {
        throw new KeyedError("chat.error.revoke_link");
      }
      forgetInviteLinkSecret(base64UrlEncodeNoPad(secrets.accountId), link.blobIdSlug);
      setLinks(links().filter((l) => l.blobIdSlug !== link.blobIdSlug));
      setNotice(() => t("invite.links.revoked"));
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
                    <Show when={row.url}>
                      {(url) => (
                        <>
                          <span class="lk-link-text" title={url()}>
                            {url().replace(/^https?:\/\//, "")}
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
                        </>
                      )}
                    </Show>
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
