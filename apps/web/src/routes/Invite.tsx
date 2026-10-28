// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `/i/:slug#<secret>` — the contact invite page
 * (`docs/ux/contact-invite-link.md` section 3, ADR-0079).
 *
 * The link is the inviter's consent: pressing **Add** makes the two people
 * contacts with nothing more to do. The page says who invites by the name
 * the link states (sealed inside the capability, ADR-0079 section 1) and
 * shows no key material: no fingerprint, no safety number, no expiry and no
 * use count. The states are one pure decision (`contacts/invite-page.ts`):
 * signed out (who invites, with Create an account / I already have an
 * account; a dead link says so), loading,
 * rate-limited, own link, already a contact, and the default Add / Not now.
 * A link that does not work (revoked, expired, used up, unknown, damaged)
 * has no page: the visitor lands on the Contacts screen (amended
 * 2026-10-19).
 *
 * **Add** sends the inviter a contact request carrying the link answer (the
 * link's id and secret, sealed to the inviter: ADR-0079 section 2), so the
 * inviter's device takes it by itself; this account records the inviter as
 * a pending contact at once (`contacts/pending-contacts.ts`) and the page
 * closes into the chat with them (section 4). The record is written only
 * after the ledger's consume answered that the link still works (amended
 * 2026-10-19: a consume that answers used up or gone keeps nothing and takes
 * the visitor to Contacts), and BEFORE the request is sent, so an answer
 * that comes back at once always finds it; a send that fails removes it
 * again and the page says so.
 *
 * `bootstrap_referral` links (ADR-0005 section 13.4) press Add by themselves
 * once the visitor is signed in: the click on the link is the consent.
 *
 * A public route: an invite URL can be opened by anyone, signed in or not.
 */
import {
  createMemo,
  createResource,
  createSignal,
  Match,
  onCleanup,
  onMount,
  Show,
  Switch,
  type JSX,
} from "solid-js";
import { A, useLocation, useNavigate, useParams } from "@solidjs/router";
import type { InviteCapability as WasmInviteCapability } from "../../wasm/lakko_ffi_wasm";
import { useAuth } from "../auth/AuthContext";
import { buildAuthUrlWithReturn } from "../auth/return-url";
import { ContactsClient } from "../contacts/client";
import { establishedContactOfAccount, pinKeysOf } from "../contacts/known-contact-request";
import {
  accountKeysOf,
  loadAccepted,
  restoreRemovedContact,
  subscribeAcceptedChanged,
} from "../contacts/local-store";
import { readVerifiedBinding } from "../contacts/verified-binding";
import { base64Decode, base64Encode, base64UrlEncodeNoPad } from "../auth/account-id";
import { groupIdHex } from "../devices/sync/state";
import { t } from "../l10n";
import { createLiveText } from "../l10n/live-text";
import { inviteOpenFailure, invitePageView, type InvitePageView } from "../contacts/invite-page";
import { addThroughInviteLink, answerAsKnownContact } from "../contacts/invite-add";
import { DiscoveryClient, isDiscoveryRateLimited } from "../discovery/client";
import {
  INVITE_LINK_RATE_LIMITED,
  INVITE_LINK_TRY_AGAIN,
  inviteLinkWidths,
  isInviteLinkUnreachable,
  openInviteLinkFromUrl,
  parseInviteLinkFragment,
} from "../contacts/invite-link";
import {
  pendingContacts,
  updatePendingContacts,
  withPendingContact,
  withoutPendingContact,
} from "../contacts/pending-contacts";
import { readIsActive } from "../chat/membership-ended";
import { useChats } from "../chat/ChatContext";
import { conversationKindOf } from "../chat/conversation-kind";
import { personKeysOf } from "../chat/person-key";
import { useProfiles } from "../profile/ProfileContext";
import { Avatar } from "../ui/Avatar";
import { loadWasm } from "../wasm";
import { paths } from "./paths";
import { PublicLayout } from "../ui/PublicLayout";

interface DecodedInvite {
  cap: WasmInviteCapability;
  /** The 6-byte ledger id the slug names (every link is a ledger pointer). */
  blobId: Uint8Array;
  /** The plaintext capability wire (the joiner tag is computed over it). */
  capabilityBytes: Uint8Array;
  /** The 12-byte link secret from the fragment: the link answer's secret. */
  linkSecret: Uint8Array;
}

const RATE_LIMITED = "rate-limited" as const;
/** The link could not be fetched or opened for a reason that says nothing
 *  about the link: the network, a server error. */
const OPEN_FAILED = "open-failed" as const;
const DEAD = "dead" as const;

export default function Invite(): JSX.Element {
  const params = useParams<{ slug: string }>();
  const location = useLocation();
  const auth = useAuth();
  const chats = useChats();
  const profiles = useProfiles();
  const navigate = useNavigate();
  // The contact list can change while the page is open (the inviter's answer
  // to an earlier Add lands): the page's decision is read again then.
  const [acceptedVersion, setAcceptedVersion] = createSignal(0);
  onCleanup(subscribeAcceptedChanged(() => setAcceptedVersion((v) => v + 1)));

  const signedIn = (): boolean => auth.state().kind === "session" && auth.identitySecrets() !== null;
  // The link is opened signed out too (amended 2026-10-26): the ledger serves
  // the sealed blob without a session and the secret opens it here, so a
  // visitor with no account sees who invites them. Nothing is consumed.
  const slugSignal = createMemo(() => ({ slug: params.slug, hash: location.hash }));
  const discovery = new DiscoveryClient();
  discovery.setSessionToken(auth.identitySecrets()?.sessionToken ?? null);
  const [decoded, { refetch }] = createResource(
    slugSignal,
    async ({ slug, hash }): Promise<DecodedInvite | typeof RATE_LIMITED | typeof OPEN_FAILED | typeof DEAD> => {
      try {
        const wasm = await loadWasm();
        const widths = inviteLinkWidths(wasm);
        const opened = await openInviteLinkFromUrl({
          slug,
          hash,
          widths,
          what: "invite",
          fetchPointer: (blobId) => discovery.fetchInvite(blobId),
          open: (sealed, secret) => wasm.InviteCapability.openLink(sealed, secret),
        });
        // A link whose id is unknown, revoked or expired reads as no longer
        // working (UX section 3).
        if (opened.kind === "not-found") return DEAD;
        const fragment = parseInviteLinkFragment(hash, widths);
        if (fragment.kind !== "secret") return DEAD;
        const cap = opened.capability;
        return { cap, blobId: opened.blobId, capabilityBytes: cap.wire, linkSecret: fragment.secret };
      } catch (err) {
        // A server error or the network says nothing about the link: it is
        // offered again (seen on a local stack 2026-10-23: a server error on
        // the fetch sent a person who had just opened a working link to
        // Contacts with no word of why).
        const failure = inviteOpenFailure(err, {
          rateLimited: isDiscoveryRateLimited,
          unreachable: isInviteLinkUnreachable,
        });
        if (failure === "rate-limited") return RATE_LIMITED;
        if (failure === "dead") return DEAD;
        // eslint-disable-next-line no-console
        console.warn("invite: link did not open", err instanceof Error ? err.name : "");
        return OPEN_FAILED;
      }
    },
  );
  const invite = (): DecodedInvite | null => {
    const d = decoded();
    return d === undefined || d === RATE_LIMITED || d === OPEN_FAILED || d === DEAD ? null : d;
  };

  /** The visitor's contact row for the inviter, when they already are one:
   *  the pin or a verified binding naming the row's key, never a row whose
   *  account id came only from a peer's own profile. */
  const existingContactKeyB64 = (cap: WasmInviteCapability): string | null => {
    const ownerAccountIdB64 = base64Encode(cap.ownerAccountId);
    const existing = establishedContactOfAccount(loadAccepted(), groupIdHex(cap.ownerAccountId), {
      pinKeysB64: pinKeysOf(accountKeysOf(ownerAccountIdB64)),
      bindingKeyB64: readVerifiedBinding(ownerAccountIdB64)?.keyB64 ?? null,
    });
    return existing?.signaturePubkeyB64 ?? null;
  };

  const view = createMemo((): InvitePageView => {
    const d = decoded();
    if (decoded.loading || d === undefined) return { kind: "loading" };
    if (d === RATE_LIMITED) return { kind: "rate-limited" };
    if (d === OPEN_FAILED) return { kind: "open-failed" };
    if (d === DEAD) return signedIn() ? { kind: "dead" } : { kind: "signed-out-dead" };
    const s = auth.state();
    const own =
      signedIn() && s.kind === "session" && base64Encode(d.cap.ownerAccountId) === s.account.accountId;
    acceptedVersion();
    const ownerHex = groupIdHex(d.cap.ownerAccountId);
    return invitePageView({
      link: "opened",
      expired: !d.cap.isValidNow(BigInt(Date.now())),
      inviterDisplayName: d.cap.inviterDisplayName ?? null,
      inviterUsername: d.cap.inviterUsername ?? null,
      label: d.cap.label,
      signedIn: signedIn(),
      isOwn: own,
      existingContactKeyB64: signedIn() ? existingContactKeyB64(d.cap) : null,
      pendingInviter: pendingContacts().some((p) => p.accountIdHex === ownerHex),
    });
  });

  const nameOr = (name: string | null): string => name ?? t("invite.contact.page.unnamed");
  /** A contact's name as this account knows it, else the link's. */
  const alreadyName = (keyB64: string, linkName: string | null): string => {
    const resolved = profiles.resolve(keyB64);
    return resolved.source === "fingerprint" ? nameOr(linkName) : resolved.name;
  };
  /** "Open chat": the one-to-one with the contact, else their page (which
   *  offers Message). */
  const chatPathOf = (keyB64: string): string => {
    const g = chats
      .groups()
      .find((c) => conversationKindOf(c.group) === "chat" && personKeysOf(c.group).includes(keyB64));
    return g !== undefined
      ? paths.chat(base64UrlEncodeNoPad(g.group.groupId))
      : paths.contact(base64UrlEncodeNoPad(base64Decode(keyB64)));
  };

  const client = new ContactsClient(undefined, undefined, {
    identityProvider: () => chats.identity(),
    // ADR-0048 section 2 — the request carries this account's profile.
    ownProfileEnvelope: () => profiles.ownProfileEnvelope(),
  });

  return (
    <PublicLayout>
      <Switch>
        <Match when={view().kind === "loading"}>
          <p class="form-info">{t("invite.link.loading")}</p>
        </Match>
        <Match when={view().kind === "rate-limited"}>
          <section class="lk-stack" style={{ gap: "12px" }}>
            <p>{INVITE_LINK_RATE_LIMITED()}</p>
            <div class="lk-actions">
              <button class="primary" type="button" onClick={() => void refetch()}>
                {INVITE_LINK_TRY_AGAIN()}
              </button>
            </div>
          </section>
        </Match>
        <Match when={view().kind === "open-failed"}>
          <section class="lk-stack" style={{ gap: "12px" }}>
            <p>{t("invite.contact.result.failed_title")}</p>
            <div class="lk-actions">
              <button class="primary" type="button" onClick={() => void refetch()}>
                {INVITE_LINK_TRY_AGAIN()}
              </button>
            </div>
          </section>
        </Match>
        <Match when={(() => { const v = view(); return v.kind === "signed-out" ? v : undefined; })()}>
          {(v) => (
            <InviterCard
              title={t("invite.contact.page.signed_out_title", { name: nameOr(v().name) })}
              name={nameOr(v().name)}
              username={v().username}
              label={v().label}
            >
              <p class="lk-muted">{t("invite.contact.page.about")}</p>
              <SignedOutActions returnTo={returnPath(params.slug, location.hash)} />
            </InviterCard>
          )}
        </Match>
        <Match when={view().kind === "signed-out-dead"}>
          <section class="lk-stack" style={{ gap: "12px", "align-items": "center", "text-align": "center" }}>
            <h1>{t("invite.contact.page.dead_title")}</h1>
            <p>{t("invite.contact.page.dead_body")}</p>
            <SignedOutActions returnTo={null} />
          </section>
        </Match>
        <Match when={view().kind === "dead"}>
          <Redirect to={paths.contacts} />
        </Match>
        <Match when={view().kind === "own"}>
          <DonePanel
            title={t("invite.contact.page.own_title")}
            body={t("invite.contact.page.own_body")}
          />
        </Match>
        <Match when={(() => { const v = view(); return v.kind === "already" ? v : undefined; })()}>
          {(v) => (
            <KnownContactPanel
              invite={invite() as DecodedInvite}
              name={alreadyName(v().contactKeyB64, v().name)}
              chatPath={chatPathOf(v().contactKeyB64)}
              client={client}
              discovery={discovery}
            />
          )}
        </Match>
        <Match when={(() => { const v = view(); return v.kind === "pending" ? v : undefined; })()}>
          {(v) => (
            <section class="lk-stack" style={{ gap: "12px", "align-items": "center", "text-align": "center" }}>
              <h1>{t("invite.contact.page.already_title", { name: nameOr(v().name) })}</h1>
              <div class="lk-actions">
                <button
                  class="primary"
                  type="button"
                  onClick={() => {
                    const d = invite();
                    if (d !== null) navigate(paths.pendingChat(groupIdHex(d.cap.ownerAccountId)));
                  }}
                >
                  {t("invite.contact.page.open_chat")}
                </button>
              </div>
            </section>
          )}
        </Match>
        <Match when={(() => { const v = view(); return v.kind === "default" ? v : undefined; })()}>
          {(v) => (
            <AddPanel
              invite={invite() as DecodedInvite}
              name={nameOr(v().name)}
              username={v().username}
              label={v().label}
              client={client}
              discovery={discovery}
            />
          )}
        </Match>
      </Switch>
    </PublicLayout>
  );
}

/** Navigate to `to` (replacing this entry) as soon as the state shows. */
function Redirect(props: { to: string }): JSX.Element {
  const navigate = useNavigate();
  onMount(() => navigate(props.to, { replace: true }));
  return null;
}

/** The return path through sign-in: the fragment is the link secret, without
 *  which the link opens nothing, so it is kept. */
function returnPath(slug: string, hash: string): string {
  const fragment = hash.length === 0 || hash.startsWith("#") ? hash : `#${hash}`;
  return `${paths.invite(slug)}${fragment}`;
}

/** Create an account (primary) and I already have an account, for a visitor
 *  who is not signed in. `returnTo` brings them back to the link afterwards;
 *  `null` for a link that no longer works. */
function SignedOutActions(props: { returnTo: string | null }): JSX.Element {
  const href = (path: string): string =>
    props.returnTo === null ? path : buildAuthUrlWithReturn(path, props.returnTo);
  return (
    <div class="lk-actions">
      <A href={href(paths.signup)}>
        <button class="primary" type="button">
          {t("invite.contact.page.create_account")}
        </button>
      </A>
      <A href={href(paths.login)}>
        <button type="button">{t("invite.contact.page.have_account")}</button>
      </A>
    </div>
  );
}

function DonePanel(props: { title: string; body: string }): JSX.Element {
  const navigate = useNavigate();
  return (
    <section class="lk-stack" style={{ gap: "12px", "align-items": "center", "text-align": "center" }}>
      <h1>{props.title}</h1>
      <p>{props.body}</p>
      <div class="lk-actions">
        <button class="primary" type="button" onClick={() => navigate(paths.home)}>
          {t("invite.contact.page.done")}
        </button>
      </div>
    </section>
  );
}

/** Avatar, "Add Anna to your contacts?", `@anna`, the label: the head every
 *  state that names the inviter shares. */
function InviterCard(props: {
  name: string;
  username: string | null;
  label: string | null;
  /** The heading; "Add Anna to your contacts?" when not given. */
  title?: string;
  /** ADR-0084 - the inviter's signature key: their photo when this device
   *  already holds their profile (the link's own preview carries none). */
  personKey?: string | undefined;
  children: JSX.Element;
}): JSX.Element {
  return (
    <section class="lk-stack" style={{ gap: "12px", "align-items": "center", "text-align": "center" }}>
      <Avatar label={props.name} id={props.name} personKey={props.personKey} />
      <h1>{props.title ?? t("invite.contact.page.title", { name: props.name })}</h1>
      <Show when={props.username}>{(u) => <p class="lk-row-sub">@{u()}</p>}</Show>
      <Show when={props.label}>{(l) => <p>{l()}</p>}</Show>
      {props.children}
    </section>
  );
}

function AddPanel(props: {
  invite: DecodedInvite;
  name: string;
  username: string | null;
  label: string | null;
  client: ContactsClient;
  discovery: DiscoveryClient;
}): JSX.Element {
  const auth = useAuth();
  const navigate = useNavigate();
  const chats = useChats();
  const cap = props.invite.cap;
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createLiveText();

  /** A live one-to-one with the inviter, when the visitor removed them from
   *  their contacts and the conversation remains (2026-10-24). Add then puts
   *  them back with it: the inviter's device answers such a request by
   *  restoring, never by founding, so a pending contact would wait for an
   *  answer that is never sent. */
  const liveChatWithOwner = (): { readonly groupIdB64: string; readonly href: string } | null => {
    const ownerKeyB64 = base64Encode(cap.ownerSignaturePubkey);
    const g = chats
      .groups()
      .find(
        (c) =>
          conversationKindOf(c.group) === "chat" &&
          personKeysOf(c.group).includes(ownerKeyB64) &&
          readIsActive(c.group) === true,
      );
    return g === undefined
      ? null
      : { groupIdB64: g.groupIdB64, href: paths.chat(base64UrlEncodeNoPad(g.group.groupId)) };
  };

  // ADR-0005 section 13.4 — a referral link presses Add by itself.
  onMount(() => {
    if (cap.bootstrapReferral) void onAdd();
  });

  async function onAdd(): Promise<void> {
    const secrets = auth.identitySecrets();
    const s = auth.state();
    if (secrets === null || s.kind !== "session" || busy()) return;
    const account = s.account.accountId;
    setError(null);
    setBusy(true);
    const ownerHex = groupIdHex(cap.ownerAccountId);
    try {
      // ADR-0073 R.2: the link's owner may already be a contact under other
      // keys; the published identity passes the fail-closed gate first.
      await props.client.refuseContactRequestToUnverifiedKeys(cap.ownerAccountId, cap.ownerSignaturePubkey);
      const blobId = props.invite.blobId;
      const live = liveChatWithOwner();
      const outcome = await addThroughInviteLink({
        // The ledger's soft use count. A link this account used before is
        // sent again: the inviter's device answers it either way.
        consume: async () =>
          (await props.discovery.consumeInvite(blobId, props.invite.capabilityBytes, secrets.accountId)).kind,
        // ADR-0079 section 5 — the inviter is this account's contact from now
        // on, written only once the link was found live.
        // With a live one-to-one nothing is pending: the contact is put back
        // once the request is sent (below).
        recordPending: async () =>
          live === null &&
          (await updatePendingContacts(account, (list) =>
            withPendingContact(list, {
              accountIdHex: ownerHex,
              signaturePubkeyB64: base64Encode(cap.ownerSignaturePubkey),
              x25519PubkeyB64: base64Encode(cap.ownerX25519Pubkey),
              displayName: cap.inviterDisplayName ?? "",
              username: cap.inviterUsername ?? "",
              addedAtMs: Date.now(),
              linkIdHex: groupIdHex(cap.linkId),
            }),
          )) !== null,
        // ADR-0060 section 6 — written to the owner's closed mailbox under
        // the link's `link` grant, used for this one write and freed.
        sendRequest: () => sendLinkAnswer(props.client, secrets, props.invite),
        forgetPending: async () => {
          await updatePendingContacts(account, (list) => withoutPendingContact(list, ownerHex));
        },
      });
      if (outcome === "dead") {
        // The link died while the page was open: nothing is kept, nothing is
        // held, the visitor lands on Contacts (amended 2026-10-19).
        navigate(paths.contacts, { replace: true });
        return;
      }
      if (live !== null) {
        restoreRemovedContact({
          signatureKey: base64Encode(cap.ownerSignaturePubkey),
          accountIdHex: ownerHex,
          groupIdB64: live.groupIdB64,
          nowMs: Date.now(),
        });
        navigate(live.href);
        return;
      }
      navigate(paths.pendingChat(ownerHex));
    } catch (err) {
      setError(() =>
        isDiscoveryRateLimited(err)
          ? INVITE_LINK_RATE_LIMITED()
          : t("invite.contact.page.add_failed", { name: props.name }),
      );
    } finally {
      setBusy(false);
    }
  }

  return (
    <>
      <InviterCard
        name={props.name}
        username={props.username}
        label={props.label}
        personKey={base64Encode(props.invite.cap.ownerSignaturePubkey)}
      >
        <div class="lk-actions">
          <button class="primary" type="button" disabled={busy()} onClick={() => void onAdd()}>
            <Show when={busy()} fallback={t("invite.contact.page.add")}>
              <InlineSpinner />
            </Show>
          </button>
          <button type="button" disabled={busy()} onClick={() => navigate(paths.home)}>
            {t("invite.contact.page.not_now")}
          </button>
        </div>
        <Show when={error()}>{(message) => <p class="form-error">{message()}</p>}</Show>
      </InviterCard>
    </>
  );
}

/**
 * The request carrying the link answer (ADR-0079 section 2): written to the
 * owner's closed mailbox under the link's `link` grant (ADR-0060 section 6),
 * used for this one write and freed. The inviter's device takes it by itself.
 */
async function sendLinkAnswer(
  client: ContactsClient,
  secrets: NonNullable<ReturnType<ReturnType<typeof useAuth>["identitySecrets"]>>,
  invite: DecodedInvite,
): Promise<void> {
  const cap = invite.cap;
  const linkGrant = cap.linkGrant;
  try {
    await client.sendContactRequestToBundle(
      secrets,
      cap.ownerAccountId,
      cap.ownerSignaturePubkey,
      cap.ownerX25519Pubkey,
      new Uint8Array(0),
      {
        writeGrant: linkGrant,
        linkAnswer: { linkId: cap.linkId, linkSecret: invite.linkSecret },
      },
    );
  } finally {
    linkGrant.free();
  }
}

/**
 * "Anna is already in your contacts" and **Open chat**
 * (`docs/ux/contact-invite-link.md` section 3, amended 2026-10-24). The link
 * still delivers its answer: an inviter who removed the visitor and sent a
 * new link gets them back on their device, with the conversation they have.
 * Nothing is kept on this side; the chat opens once the answer is written,
 * or at once when the link no longer works.
 */
function KnownContactPanel(props: {
  invite: DecodedInvite;
  name: string;
  chatPath: string;
  client: ContactsClient;
  discovery: DiscoveryClient;
}): JSX.Element {
  const auth = useAuth();
  const navigate = useNavigate();
  const cap = props.invite.cap;
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createLiveText();

  async function onOpenChat(): Promise<void> {
    const secrets = auth.identitySecrets();
    if (secrets === null || busy()) return;
    setError(null);
    setBusy(true);
    try {
      await props.client.refuseContactRequestToUnverifiedKeys(cap.ownerAccountId, cap.ownerSignaturePubkey);
      const blobId = props.invite.blobId;
      await answerAsKnownContact({
        consume: async () =>
          (await props.discovery.consumeInvite(blobId, props.invite.capabilityBytes, secrets.accountId)).kind,
        sendRequest: () => sendLinkAnswer(props.client, secrets, props.invite),
      });
      navigate(props.chatPath);
    } catch (err) {
      setError(() =>
        isDiscoveryRateLimited(err)
          ? INVITE_LINK_RATE_LIMITED()
          : t("invite.contact.page.answer_failed", { name: props.name }),
      );
    } finally {
      setBusy(false);
    }
  }

  return (
    <section class="lk-stack" style={{ gap: "12px", "align-items": "center", "text-align": "center" }}>
      <h1>{t("invite.contact.page.already_title", { name: props.name })}</h1>
      <div class="lk-actions">
        <button class="primary" type="button" disabled={busy()} onClick={() => void onOpenChat()}>
          <Show when={busy()} fallback={t("invite.contact.page.open_chat")}>
            <InlineSpinner />
          </Show>
        </button>
      </div>
      <Show when={error()}>{(message) => <p class="form-error">{message()}</p>}</Show>
    </section>
  );
}

/** The spinner shown in place of a button's label while it works. */
function InlineSpinner(): JSX.Element {
  return (
    <svg class="lk-spinner" width={18} height={18} viewBox="0 0 24 24" fill="none" aria-hidden="true">
      <circle cx="12" cy="12" r="9" stroke="currentColor" stroke-width="3" stroke-linecap="round" stroke-dasharray="40 60">
        <animateTransform
          attributeName="transform"
          attributeType="XML"
          type="rotate"
          from="0 12 12"
          to="360 12 12"
          dur="0.8s"
          repeatCount="indefinite"
        />
      </circle>
    </svg>
  );
}
