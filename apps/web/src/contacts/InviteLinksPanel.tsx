// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * "Invite links" (docs/ux/client-redesign.md section 5.3): the account's
 * active links, each a row with Copy, Share and Revoke, and a "Create link"
 * form. Never a raw URL in a textarea.
 *
 * The create path is the one that lived in `routes/Contacts.tsx`: register
 * the link's reach key (ADR-0058 section 6) before anything else, compose and
 * seal the capability in the core (2026-09-28 security pass), upload the
 * sealed blob to the discovery service for the short pointer URL, fall back
 * to the self-contained inline URL only on a network failure (a rejected
 * upload is a real error; the same bytes would be rejected again by whoever
 * opens them). The link secret rides in the URL fragment, which the ledger
 * never sees, so this device keeps it (`minted-links.ts`) to rebuild the URL
 * of a listed link. The list is the owner query the old "Active invites" card
 * made, run when this panel opens and after every change, never behind a
 * button.
 */
import { createSignal, For, onMount, Show, type JSX } from "solid-js";

import { useAuth } from "../auth/AuthContext";
import { base64Encode, base64UrlDecodeNoPad, base64UrlEncodeNoPad } from "../auth/account-id";
import { useChats } from "../chat/ChatContext";
import { DiscoveryClient, type ActiveInvite } from "../discovery/client";
import { Icon } from "../ui/icons";
import { loadWasm } from "../wasm";
import { formatContactsError } from "./errors";
import { reachSession } from "../reach/reach-service";
import { forgetMintedLink, mintedLinkFor, rememberMintedLink, type MintedLink } from "./minted-links";
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
  /** `null` for a link another device minted: the secret is not here. */
  readonly url: string | null;
  readonly title: string;
  readonly meta: string;
  readonly revocable: boolean;
  readonly blobIdSlug: string | null;
}

/** `null` when the listed id is not a ledger id of the current width (a row
 *  the server should not have sent); the row is then left out rather than
 *  breaking the list. A link this device did not mint is listed without a
 *  URL (its secret lives only on the device that minted it). */
function rowOf(
  invite: ActiveInvite,
  nowMs: number,
  widths: InviteLinkWidths,
  minted: MintedLink | undefined,
): LinkRow | null {
  let url: string | null = null;
  try {
    const blobId = base64UrlDecodeNoPad(invite.blobIdSlug);
    if (blobId.length !== widths.blobIdLen) return null;
    if (minted !== undefined) url = composePointerInviteUrl(blobId, minted.linkSecret, widths);
  } catch {
    return null;
  }
  const parts = [
    describeInviteCreated(invite.createdAt),
    describeInviteExpiry(invite.expiryMs, nowMs),
    describeInviteUses(invite.usedCount, invite.maxUses),
  ].filter((p) => p.length > 0);
  if (invite.bootstrapReferral) parts.push("For someone new to Lakko");
  if (url === null) parts.push("Made on another device");
  return {
    key: invite.blobIdSlug,
    url,
    title: invite.label.length > 0 ? invite.label : "Invite link",
    meta: parts.join(" · "),
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
  /** The local half of each listed link this device minted, by ledger slug. */
  const [minted, setMinted] = createSignal<ReadonlyMap<string, MintedLink>>(new Map());
  const [widths, setWidths] = createSignal<InviteLinkWidths | null>(null);
  const [loaded, setLoaded] = createSignal(false);
  const [inlineLink, setInlineLink] = createSignal<LinkRow | null>(null);
  const [error, setError] = createSignal<string | null>(null);
  const [notice, setNotice] = createSignal<string | null>(null);
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
            .map((i) => rowOf(i, now, w, minted().get(i.blobIdSlug)))
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
        throw new Error("Sign in again to see your invite links.");
      }
      setWidths(inviteLinkWidths(await loadWasm()));
      const result = await discovery.listInvitesByOwner(secrets.accountId, id.ed25519Seed);
      // "not found or not owner" means no row matches this account: no links.
      const listed = result.kind === "list" ? result.invites : [];
      const accountIdB64 = base64Encode(secrets.accountId);
      const local = new Map<string, MintedLink>();
      for (const invite of listed) {
        const m = await mintedLinkFor(accountIdB64, "contact", invite.blobIdSlug);
        if (m !== null) local.set(invite.blobIdSlug, m);
      }
      setMinted(local);
      setInvites(listed);
      setLoaded(true);
    } catch (err) {
      setError(formatContactsError(err));
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
        throw new Error("Sign in again to create an invite link.");
      }
      // ADR-0058 section 6: the link's reach key is registered before the
      // link exists; a link shared before its key is registered does not
      // work. No reach session, or a failed registration, fails the mint.
      const session = reachSession();
      if (session === null) {
        throw new Error("Lakko is still starting up. Try again in a moment.");
      }
      const wasm = await loadWasm();
      const w = widths() ?? inviteLinkWidths(wasm);
      const expiryMs = expiryPresetToMs(expiry(), Date.now());
      const uses = maxUsesPresetToValue(maxUses());
      const linkId = wasm.generateLinkId();
      const linkGrant = await session.newLink(linkId, expiryMs === 0n ? null : Number(expiryMs));
      let mint: import("../../wasm/lakko_ffi_wasm").InviteLinkMint;
      try {
        mint = wasm.InviteCapability.compose(
          id.ed25519Seed,
          secrets.accountId,
          id.x25519PublicKey,
          expiryMs,
          uses,
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
      const issuerPubkey = mint.issuerPubkey;
      mint.free();
      try {
        const blobId = await discovery.uploadInvite({
          sealed,
          issuerPubkey,
          expiryMs,
          maxUses: uses,
          label: label(),
          bootstrapReferral: bootstrapReferral(),
        });
        await rememberMintedLink(
          base64Encode(secrets.accountId),
          "contact",
          base64UrlEncodeNoPad(blobId),
          { linkSecret, linkId },
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
        console.warn("invite upload network-failed; falling back to inline URL", uploadErr);
        setInlineLink({
          key: "inline",
          url: composeInlineInviteUrl(sealed, linkSecret, w),
          title: label().length > 0 ? label() : "Invite link",
          meta: "Lakko could not shorten this link, so it is long. It works the same way and cannot be revoked.",
          revocable: false,
          blobIdSlug: null,
        });
      }
      setCreating(false);
      setLabel("");
      setBootstrapReferral(false);
    } catch (err) {
      setError(formatContactsError(err));
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
      setError(formatContactsError(err));
    }
  }

  async function onShare(row: LinkRow): Promise<void> {
    if (row.url === null) return;
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
      const secrets = auth.identitySecrets();
      if (id === null || secrets === null) throw new Error("Sign in again to revoke an invite link.");
      const slug = row.blobIdSlug;
      const result = await discovery.revokeInvite(base64UrlDecodeNoPad(slug), id.ed25519Seed);
      // ADR-0058 section 6: the link's reach key goes with it, so a request
      // written under the link is dropped even by a joiner who fetched it.
      const local = minted().get(slug);
      if (local !== undefined) {
        await reachSession()?.revokeLink(local.linkId);
        await forgetMintedLink(base64Encode(secrets.accountId), "contact", slug);
      }
      // Both outcomes mean the row is gone server-side.
      setInvites(invites().filter((i) => i.blobIdSlug !== row.blobIdSlug));
      setNotice(
        result.kind === "revoked" ? "Link revoked." : `"${row.title}" had already been removed.`,
      );
    } catch (err) {
      setError(formatContactsError(err));
    } finally {
      setBusyKey(null);
    }
  }

  return (
    <section class="lk-card">
      <h2>Invite links</h2>
      <p class="lk-card-desc">
        Anyone who opens one of your links can send you a contact request until it expires or is
        used up.
      </p>

      <Show when={error()}>{(m) => <div class="form-error">{m()}</div>}</Show>
      <Show when={notice()}>{(m) => <div class="form-info">{m()}</div>}</Show>

      <Show
        when={creating()}
        fallback={
          <div class="lk-actions">
            <button type="button" class="primary" onClick={() => setCreating(true)}>
              <Icon name="plus" size={18} />
              Create link
            </button>
          </div>
        }
      >
        <form onSubmit={(e) => void onCreate(e)} class="lk-stack" style={{ gap: "12px" }}>
          <label>
            Label (optional)
            <input
              type="text"
              value={label()}
              onInput={(e) => setLabel(e.currentTarget.value)}
              placeholder="Shown to whoever opens the link"
              maxlength={INVITE_LABEL_MAX_LEN}
              disabled={createBusy()}
            />
          </label>
          <label>
            Expires
            <select
              value={expiry()}
              onChange={(e) => setExpiry(e.currentTarget.value as ExpiryPreset)}
              disabled={createBusy()}
            >
              <option value="1d">In 1 day</option>
              <option value="7d">In 7 days</option>
              <option value="30d">In 30 days</option>
              <option value="never">Never</option>
            </select>
          </label>
          <label>
            Uses
            <select
              value={maxUses()}
              onChange={(e) => setMaxUses(e.currentTarget.value as MaxUsesPreset)}
              disabled={createBusy()}
            >
              <option value="single">One person</option>
              <option value="ten">Up to 10 people</option>
              <option value="unlimited">Unlimited</option>
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
            <span>For someone without a Lakko account yet</span>
          </label>
          <div class="lk-actions">
            <button class="primary" type="submit" disabled={createBusy()}>
              {createBusy() ? "Creating..." : "Create link"}
            </button>
            <button type="button" class="ghost" disabled={createBusy()} onClick={() => setCreating(false)}>
              Cancel
            </button>
          </div>
        </form>
      </Show>

      <Show when={loaded()} fallback={<p class="lk-muted">Loading...</p>}>
        <Show when={rows().length > 0} fallback={<p class="lk-muted">No active links.</p>}>
          <ul class="lk-list lk-invite-list" aria-label="Active invite links">
            <For each={rows()}>
              {(row) => (
                <li class="lk-invite-item">
                  <div class="lk-invite-title">
                    <strong>{row.title}</strong>
                    <span class="lk-meta">{row.meta}</span>
                  </div>
                  <div class="lk-link-row" role="group" aria-label={row.title}>
                    <Show when={row.url}>
                      {(url) => (
                        <>
                          <span class="lk-link-text" title={url()}>
                            {url().replace(/^https?:\/\//, "")}
                          </span>
                          <button
                            type="button"
                            class="lk-icon-button"
                            title={copiedKey() === row.key ? "Copied" : "Copy link"}
                            aria-label={copiedKey() === row.key ? "Copied" : "Copy link"}
                            onClick={() => void onCopy(row)}
                          >
                            <Icon name={copiedKey() === row.key ? "check" : "copy"} size={16} />
                          </button>
                          <button
                            type="button"
                            class="lk-icon-button"
                            title="Share"
                            aria-label="Share"
                            onClick={() => void onShare(row)}
                          >
                            <Icon name="share" size={16} />
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
                        {busyKey() === row.key ? "Revoking..." : "Revoke"}
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
