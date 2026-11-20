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
 * reach scope id and its own revocation seed). Any failed upload, a network
 * one included, mints no link and shows the error; Create stays open for
 * another try (the inline form was removed on 2026-10-19).
 *
 * ADR-0078 section 10: the ledger rows name no owner, so there is no owner
 * query. ADR-0079 section 4 replaced "the links made on this device": the
 * list is the account's link-record register (`devices/sync/link-records.ts`),
 * which every device of the account holds, so a link made on another device
 * is listed, copied and revoked here too. Since the 2026-10-17 rework the
 * register also carries the ledger row's revocation seed, so revoking on any
 * device marks the register revoked (final) and deletes the ledger row at
 * once with that seed (`own-links.ts` `deleteRevokedLinkRow`); a deletion
 * the ledger could not be reached for is owed and retried by this device.
 * Only the revoking device deletes the row, once; the other devices only
 * forget their local record. No device of the account takes a request
 * through the link from that moment.
 */
import { createSignal, For, onMount, Show, type JSX } from "solid-js";

import { useAuth } from "../auth/AuthContext";
import { base64Encode, base64UrlEncodeNoPad } from "../auth/account-id";
import { useChats } from "../chat/ChatContext";
import { DiscoveryClient } from "../discovery/client";
import { BrandedQr } from "../ui/BrandedQr";
import { Icon } from "../ui/icons";
import { requestConfirm } from "../ui/confirm-dialog";
import { LakkoPicker } from "../ui/LakkoPicker";
import { loadWasm } from "../wasm";
import { joinFacts, t } from "../l10n";
import { KeyedError } from "../l10n/keyed-error";
import { createLiveText } from "../l10n/live-text";
import { contactsErrorText } from "./errors";
import { reachSession } from "../reach/reach-service";
import {
  listMintedLinks,
  rememberMintedLink,
  type MintedLinkRow,
} from "./minted-links";
import {
  deleteRevokedLinkRow,
  forgetMintedLinkRecord,
  newRevocationKey,
  setOwedRowDeletion,
} from "./own-links";
import { hexOf } from "../devices/sync/link-records";
import {
  appendInviteLinkSecret,
  inviteLinkWidths,
  type InviteLinkWidths,
} from "./invite-link";
import { forgetInviteLinkSecret } from "./invite-link-secrets";
import {
  composePointerInviteUrl,
  CUSTOM_MAX_USES,
  customExpiryMs,
  customMaxUses,
  DEFAULT_LINK_EXPIRY,
  DEFAULT_LINK_USES,
  expiryPresetToMs,
  termsFormOf,
  type ExpiryUnit,
  INVITE_LABEL_MAX_LEN,
  maxUsesPresetToValue,
  type ExpiryPreset,
  type MaxUsesPreset,
} from "./invite";
import { describeInviteCreated, describeInviteExpiry, describeInviteUsage } from "./invite-describe";
import { useDeviceGroupSync } from "../devices/sync/DeviceGroupSyncContext";
import { listedContactLinks, type LinkRecordModel } from "../devices/sync/link-records";
import { useProfiles } from "../profile/ProfileContext";
import { displayLocale } from "../l10n/display-locale";

/** A link this panel can show: one in the account's link-record register. */
interface LinkRow {
  readonly key: string;
  readonly url: string | null;
  /** The owner's label, or `null` for an unlabelled link (no title line:
   *  the facts lead). */
  readonly label: string | null;
  /** The facts under the title, rendered in the current language. */
  readonly meta: () => string;
  /** The register record the revoke acts on. */
  readonly record: LinkRecordModel;
}

/** A row for one register record, or `null` for a ledger id of another
 *  width. Pure. */
function rowOfRecord(record: LinkRecordModel, nowMs: number, widths: InviteLinkWidths): LinkRow | null {
  let url: string;
  try {
    const pointer = composePointerInviteUrl(record.ledgerId, widths);
    url = `${pointer}${appendInviteLinkSecret("", record.linkSecret, widths)}`;
  } catch {
    return null;
  }
  const meta = (): string =>
    joinFacts(
      [
        describeInviteCreated(new Date(record.mintedAtMs).toISOString()),
        describeInviteExpiry(record.expiryMs, nowMs),
        describeInviteUsage(record.uses, record.maxUses),
      ].filter((p) => p.length > 0),
    );
  return {
    key: record.linkIdHex,
    url,
    label: record.label.length > 0 ? record.label : null,
    meta,
    record,
  };
}

function bytesEqual(a: Uint8Array, b: Uint8Array): boolean {
  return a.length === b.length && a.every((v, k) => v === b[k]);
}

export function InviteLinksPanel(): JSX.Element {
  const auth = useAuth();
  const chats = useChats();
  const sync = useDeviceGroupSync();
  const profiles = useProfiles();
  // Read at request time: a token captured when the panel opened is stale
  // once the access token is refreshed (900 s).
  const discovery = new DiscoveryClient(undefined, {
    sessionToken: () => auth.identitySecrets()?.sessionToken ?? null,
  });

  const [links, setLinks] = createSignal<MintedLinkRow[]>([]);
  const [widths, setWidths] = createSignal<InviteLinkWidths | null>(null);
  const [loaded, setLoaded] = createSignal(false);
  const [error, setError] = createLiveText();
  const [notice, setNotice] = createLiveText();
  const [busyKey, setBusyKey] = createSignal<string | null>(null);
  const [copiedKey, setCopiedKey] = createSignal<string | null>(null);

  /** The row whose terms are being edited (UX section 9), or `null`. */
  const [editingKey, setEditingKey] = createSignal<string | null>(null);
  const [label, setLabel] = createSignal("");
  const [expiry, setExpiry] = createSignal<ExpiryPreset | "custom">("7d");
  const [customExpiryAmount, setCustomExpiryAmount] = createSignal("");
  const [customExpiryUnit, setCustomExpiryUnit] = createSignal<ExpiryUnit>("days");
  const [maxUses, setMaxUses] = createSignal<MaxUsesPreset | "custom">("single");
  const [customUses, setCustomUses] = createSignal("");

  /** The expiry the form describes, or `null` while a custom period is
   *  out of range (Create stays disabled). */
  const chosenExpiryMs = (nowMs: number): bigint | null => {
    const e = expiry();
    return e === "custom"
      ? customExpiryMs(customExpiryAmount(), customExpiryUnit(), nowMs)
      : expiryPresetToMs(e, nowMs);
  };
  const chosenMaxUses = (): number | null => {
    const u = maxUses();
    return u === "custom" ? customMaxUses(customUses()) : maxUsesPresetToValue(u);
  };
  const formValid = (): boolean => chosenExpiryMs(Date.now()) !== null && chosenMaxUses() !== null;
  const [qrRow, setQrRow] = createSignal<LinkRow | null>(null);
  const [createBusy, setCreateBusy] = createSignal(false);
  const [saveBusy, setSaveBusy] = createSignal(false);

  /** Open the terms form of `row`, prefilled with the link's terms. */
  function startEdit(row: LinkRow): void {
    const form = termsFormOf(row.record);
    setLabel(form.label);
    setExpiry(form.expiry);
    setCustomExpiryAmount(form.customExpiryAmount);
    setCustomExpiryUnit(form.customExpiryUnit);
    setMaxUses(form.maxUses);
    setCustomUses(form.customUses);
    setError(null);
    setNotice(null);
    setEditingKey(row.key);
  }

  const rows = (): LinkRow[] => {
    const now = Date.now();
    const w = widths();
    // ADR-0079: the register's live contact links (a used-up single-use link
    // and a revoked one leave the list), newest first.
    return w === null
      ? []
      : listedContactLinks(sync.linkRecords(), now)
          .map((r) => rowOfRecord(r, now, w))
          .filter((r): r is LinkRow => r !== null);
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

  /** Mint one link with these terms: reach key, sealed capability, ledger
   *  row, this device's record and the synced register. Throws on any
   *  failure, having minted nothing. */
  async function mintLink(terms: {
    readonly label: string;
    readonly expiryMs: bigint;
    readonly maxUses: number;
  }): Promise<void> {
    {
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
      const expiryMs = terms.expiryMs;
      const maxUsesValue = terms.maxUses;
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
          // The sender does not decide whether the other person has an
          // account: the invite page handles both.
          false,
          terms.label,
          linkId,
          linkGrant,
          // ADR-0079 section 1 — the link names its owner on the invite page.
          profiles.ownProfile().displayName ?? undefined,
          profiles.ownUsername() ?? undefined,
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
              label: terms.label,
              expiryMs: Number(expiryMs),
              maxUses: maxUsesValue,
              bootstrapReferral: false,
            },
          },
        );
        // ADR-0079 section 4 — the link joins the synced register, so every
        // device of the account recognises a request through it.
        const recorded = await sync.setLinkRecord({
          linkId,
          linkSecret,
          kind: "contact",
          ledgerId: blobId,
          revocationSeed: revocation.seed,
          expiryMs: Number(expiryMs),
          maxUses: maxUsesValue,
          mintedAtMs: Date.now(),
          label: terms.label,
        });
        // A link this device would not remember after a reload is not handed
        // out: every request through it would wait for ever.
        if (!recorded) throw new Error("invite link not recorded");
        await load();
      } catch (uploadErr) {
        // Any failure, a network one included, mints no link: nothing was
        // handed out, so the key goes with it and the error is shown.
        await session.revokeLink(linkId).catch(() => undefined);
        throw uploadErr;
      }
    }
  }

  /** Create link (UX section 9): a link with the default terms, at once. */
  async function onCreate(): Promise<void> {
    setError(null);
    setNotice(null);
    setCreateBusy(true);
    try {
      await mintLink({
        label: "",
        expiryMs: expiryPresetToMs(DEFAULT_LINK_EXPIRY, Date.now()),
        maxUses: maxUsesPresetToValue(DEFAULT_LINK_USES),
      });
    } catch (err) {
      setError(contactsErrorText(err, "chat.error.create_link"));
    } finally {
      setCreateBusy(false);
    }
  }

  /** Save new terms for `row`: a new link first, then the old one deleted,
   *  so a failure leaves the old link working. */
  async function onSave(e: SubmitEvent, row: LinkRow): Promise<void> {
    e.preventDefault();
    const expiryMs = chosenExpiryMs(Date.now());
    const maxUsesValue = chosenMaxUses();
    // The button is disabled while either is out of range.
    if (expiryMs === null || maxUsesValue === null) return;
    setError(null);
    setNotice(null);
    setSaveBusy(true);
    try {
      try {
        await mintLink({ label: label().trim(), expiryMs, maxUses: maxUsesValue });
      } catch (err) {
        setError(contactsErrorText(err, "invite.links.error.edit"));
        return;
      }
      setEditingKey(null);
      if (!(await removeLink(row.record))) setError(() => t("invite.links.error.delete"));
    } finally {
      setSaveBusy(false);
    }
  }

  async function onCopy(row: LinkRow): Promise<void> {
    if (row.url === null) return;
    try {
      // UX section 9: a copied link is no longer editable on any device,
      // marked before the text reaches the clipboard.
      await sync.markLinkShared(row.record.linkId);
      await navigator.clipboard.writeText(row.url);
      setCopiedKey(row.key);
      window.setTimeout(() => setCopiedKey((k) => (k === row.key ? null : k)), 2000);
    } catch (err) {
      console.warn("[contacts] copy failed", err);
      setError(() => t("invite.links.error.copy"));
    }
  }

  /** Delete link (UX section 9): a link nobody can hold goes at once; a
   *  copied or used one asks first. */
  async function onDelete(row: LinkRow): Promise<void> {
    if (!row.record.editable) {
      const yes = await requestConfirm({
        title: t("invite.links.delete_confirm_title"),
        body: t("invite.links.delete_confirm_body"),
        confirmLabel: t("common.delete"),
        cancelLabel: t("common.cancel"),
        tone: "danger",
      });
      if (!yes) return;
    }
    setError(null);
    setNotice(null);
    setBusyKey(row.key);
    try {
      if (await removeLink(row.record)) setNotice(() => t("invite.links.deleted"));
      else setError(() => t("invite.links.error.delete"));
    } finally {
      setBusyKey(null);
    }
  }

  /** Remove a link (the old Revoke): marked revoked in the register, its
   *  ledger row and reach key deleted. `false` when it could not be done in
   *  full; the register already refuses the link either way. */
  async function removeLink(record: LinkRecordModel): Promise<boolean> {
    try {
      const secrets = auth.identitySecrets();
      if (secrets === null) throw new KeyedError("invite.links.error.sign_in_revoke");
      // ADR-0079 (reworked 2026-10-17): the ledger row is deleted by this
      // device, the one that revokes, with the revocation seed the synced
      // record carries; no other device sends the ledger anything. The
      // deletion is owed before the register changes, so a crash in between
      // leaves it owed, and `MailboxContext` retries it (joining this call
      // while it runs, so the row is deleted once).
      const account = base64Encode(secrets.accountId);
      const linkIdHex = hexOf(record.linkId);
      setOwedRowDeletion(account, linkIdHex, true);
      // ADR-0079 section 4 — marked revoked in the register: no device of
      // the account takes a request through it from now on, whatever the
      // ledger answers.
      try {
        await sync.revokeLinkRecord(record.linkId);
      } catch (err) {
        setOwedRowDeletion(account, linkIdHex, false);
        throw err;
      }
      // A ledger that cannot be reached leaves the deletion owed; the
      // register already refuses the link on every device. The reach key
      // (ADR-0060 section 6) goes too.
      const outcome = await deleteRevokedLinkRow(discovery, account, record);
      await reachSession()?.revokeLink(record.linkId).catch(() => undefined);
      const local = links().find((l) => l.kind === "contact" && bytesEqual(l.linkId, record.linkId));
      if (local !== undefined) {
        await forgetMintedLinkRecord(account, local);
        forgetInviteLinkSecret(base64UrlEncodeNoPad(secrets.accountId), local.blobIdSlug);
        setLinks(links().filter((l) => l.blobIdSlug !== local.blobIdSlug));
      }
      return outcome !== "unreachable";
    } catch (err) {
      console.warn("[contacts] delete link failed", err instanceof Error ? err.name : "");
      return false;
    }
  }

  /** The terms form of `row` (UX section 9), shown under it while edited. */
  const termsForm = (row: LinkRow): JSX.Element => (
        <form onSubmit={(e) => void onSave(e, row)} class="lk-stack lk-invite-edit" style={{ gap: "12px" }}>
          <strong>{t("invite.links.edit_title")}</strong>
          <label>
            {t("invite.links.label_field")}
            <input
              type="text"
              value={label()}
              onInput={(e) => setLabel(e.currentTarget.value)}
              placeholder={t("invite.links.label_placeholder")}
              maxlength={INVITE_LABEL_MAX_LEN}
              disabled={saveBusy()}
            />
          </label>
          <div class="lk-picker-field">
            <span id="lk-invite-expires">{t("invite.links.expires_field")}</span>
            <LakkoPicker<ExpiryPreset | "custom">
              labelledBy="lk-invite-expires"
              value={expiry()}
              onChange={setExpiry}
              disabled={saveBusy()}
              options={[
                { value: "1d", label: t("invite.links.expires_option_days", { count: 1 }) },
                { value: "7d", label: t("invite.links.expires_option_days", { count: 7 }) },
                { value: "30d", label: t("invite.links.expires_option_days", { count: 30 }) },
                { value: "never", label: t("invite.links.expires_option_never") },
                { value: "custom", label: t("invite.links.expires_option_custom") },
              ]}
            />
          </div>
          <Show when={expiry() === "custom"}>
            <div class="lk-invite-custom">
              <label>
                {t("invite.links.custom_expiry_amount")}
                <input
                  type="number"
                  inputmode="numeric"
                  min="1"
                  step="1"
                  value={customExpiryAmount()}
                  onInput={(e) => setCustomExpiryAmount(e.currentTarget.value)}
                  disabled={saveBusy()}
                />
              </label>
              <LakkoPicker<ExpiryUnit>
                ariaLabel={t("invite.links.custom_expiry_amount")}
                value={customExpiryUnit()}
                onChange={setCustomExpiryUnit}
                disabled={saveBusy()}
                options={[
                  { value: "hours", label: t("invite.links.unit_hours") },
                  { value: "days", label: t("invite.links.unit_days") },
                ]}
              />
            </div>
            <Show
              when={
                customExpiryAmount().trim() !== "" &&
                customExpiryMs(customExpiryAmount(), customExpiryUnit(), Date.now()) === null
              }
            >
              <div class="form-error">{t("invite.links.error.custom_expiry")}</div>
            </Show>
          </Show>
          <div class="lk-picker-field">
            <span id="lk-invite-uses">{t("invite.links.uses_field")}</span>
            <LakkoPicker<MaxUsesPreset | "custom">
              labelledBy="lk-invite-uses"
              value={maxUses()}
              onChange={setMaxUses}
              disabled={saveBusy()}
              options={[
                { value: "single", label: t("invite.links.uses_option_one") },
                { value: "ten", label: t("invite.links.uses_option_up_to", { count: 10 }) },
                { value: "unlimited", label: t("invite.links.uses_option_unlimited") },
                { value: "custom", label: t("invite.links.uses_option_custom") },
              ]}
            />
          </div>
          <Show when={maxUses() === "custom"}>
            <label>
              {t("invite.links.custom_uses_count")}
              <input
                type="number"
                inputmode="numeric"
                min="1"
                max={CUSTOM_MAX_USES}
                step="1"
                value={customUses()}
                onInput={(e) => setCustomUses(e.currentTarget.value)}
                disabled={saveBusy()}
              />
            </label>
            <Show when={customUses().trim() !== "" && customMaxUses(customUses()) === null}>
              <div class="form-error">
                {t("invite.links.error.custom_uses", {
                  max: CUSTOM_MAX_USES.toLocaleString(displayLocale()),
                })}
              </div>
            </Show>
          </Show>
          <p class="lk-muted">{t("invite.links.edit_note")}</p>
          <div class="lk-actions">
            <button class="primary" type="submit" disabled={saveBusy() || !formValid()}>
              {t("common.save")}
            </button>
            <button type="button" class="ghost" disabled={saveBusy()} onClick={() => setEditingKey(null)}>
              {t("common.cancel")}
            </button>
          </div>
        </form>
  );

  return (
    <section class="lk-card">
      <h2>{t("contacts.invite_links.title")}</h2>
      <p class="lk-card-desc">{t("invite.links.desc")}</p>

      <Show when={error()}>{(m) => <div class="form-error">{m()}</div>}</Show>
      <Show when={notice()}>{(m) => <div class="form-info">{m()}</div>}</Show>

      <div class="lk-actions">
        <button type="button" class="primary" disabled={createBusy()} onClick={() => void onCreate()}>
          <Icon name="plus" size={20} />
          {createBusy() ? t("chats.new_group.creating") : t("invite.links.create")}
        </button>
      </div>

      <Show when={loaded()} fallback={<p class="lk-muted">{t("common.loading")}</p>}>
        <Show when={rows().length > 0} fallback={<p class="lk-muted">{t("invite.links.empty")}</p>}>
          <ul class="lk-list lk-invite-list" aria-label={t("invite.links.list")}>
            <For each={rows()}>
              {(row) => (
                <li class="lk-invite-item">
                  <div class="lk-invite-title">
                    <Show when={row.label}>{(label) => <strong>{label()}</strong>}</Show>
                    <span class="lk-meta">{row.meta()}</span>
                  </div>
                  <div class="lk-link-row" role="group" aria-label={row.label ?? row.meta()}>
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
                            <Icon name={copiedKey() === row.key ? "checkmark" : "copy"} size={20} />
                          </button>
                          <button
                            type="button"
                            class="lk-icon-button"
                            title={t("invite.links.show_qr")}
                            aria-label={t("invite.links.show_qr")}
                            onClick={() => setQrRow(row)}
                          >
                            <Icon name="qrcode" size={20} />
                          </button>
                        </>
                      )}
                    </Show>
                    <Show when={row.record.editable && editingKey() !== row.key}>
                      <button
                        type="button"
                        class="lk-icon-button"
                        title={t("invite.links.edit")}
                        aria-label={t("invite.links.edit")}
                        disabled={busyKey() !== null || saveBusy()}
                        onClick={() => startEdit(row)}
                      >
                        <Icon name="pencil" size={20} />
                      </button>
                    </Show>
                    <button
                      type="button"
                      class="lk-icon-button"
                      style={{ color: "var(--danger)" }}
                      title={t("invite.links.delete")}
                      aria-label={t("invite.links.delete")}
                      disabled={busyKey() !== null || saveBusy()}
                      onClick={() => void onDelete(row)}
                    >
                      <Icon name="trash" size={20} />
                    </button>
                  </div>
                  <Show when={editingKey() === row.key}>{termsForm(row)}</Show>
                </li>
              )}
            </For>
          </ul>
        </Show>
      </Show>

      <Show when={qrRow()}>
        {(row) => <InviteQrDialog row={row()} onClose={() => setQrRow(null)} />}
      </Show>
    </section>
  );
}

/** The link as a QR code in a modal `<dialog>` (focus trap and Escape come
 *  with `showModal`). `BrandedQr` draws its own white card in both themes:
 *  a light-on-dark code does not scan on every camera. */
function InviteQrDialog(props: { readonly row: LinkRow; readonly onClose: () => void }): JSX.Element {
  let dialog: HTMLDialogElement | undefined;
  onMount(() => {
    dialog?.showModal();
  });
  return (
    <dialog
      ref={dialog}
      class="lk-qr-dialog"
      aria-label={props.row.label ?? t("common.qr_code")}
      onClose={props.onClose}
      onClick={(e) => {
        if (e.target === e.currentTarget) dialog?.close();
      }}
    >
      <div class="lk-qr-dialog-body">
        <Show when={props.row.label}>{(label) => <strong>{label()}</strong>}</Show>
        <BrandedQr text={props.row.url ?? ""} size={264} />
        <span class="lk-meta">{props.row.meta()}</span>
        <p class="lk-muted">{t("invite.links.qr_hint")}</p>
        <div class="lk-actions">
          <button type="button" class="primary" onClick={() => dialog?.close()}>
            {t("common.done")}
          </button>
        </div>
      </div>
    </dialog>
  );
}
