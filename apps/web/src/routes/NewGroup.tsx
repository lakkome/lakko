// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `/groups/new` — name a group and pick its first members from your contacts
 * (UX spec `docs/ux/new-group.md`). Creates the group with the first pick and
 * hands the rest to the chat view, which adds them through its own add path.
 */
import { createSignal, For, onCleanup, onMount, Show, type JSX } from "solid-js";
import { useNavigate } from "@solidjs/router";

import { useAuth } from "../auth/AuthContext";
import { base64Decode, base64UrlEncodeNoPad } from "../auth/account-id";
import { useChats } from "../chat/ChatContext";
import { useGroupNames } from "../chat/GroupNameContext";
import { accountIdBytesFromHex, setPendingGroupSetup } from "../chat/pending-group-setup";
import { recordConversationKind } from "../chat/conversation-kind-store";
import { ContactsClient } from "../contacts/client";
import { loadAccepted, subscribeAcceptedChanged, type AcceptedContact } from "../contacts/local-store";
import { useProfiles } from "../profile/ProfileContext";
import { Avatar } from "../ui/Avatar";
import { Icon } from "../ui/icons";
import { contactsErrorText } from "../contacts/errors";
import { t } from "../l10n";
import { createLiveText } from "../l10n/live-text";
import { paths } from "./paths";

export const GROUP_NAME_MAX = 64;

export default function NewGroup(): JSX.Element {
  const auth = useAuth();
  const chats = useChats();
  const profiles = useProfiles();
  const groupNames = useGroupNames();
  const navigate = useNavigate();

  const client = new ContactsClient(undefined, undefined, {
    identityProvider: () => chats.identity(),
    onPersist: async () => {
      const s = auth.state();
      if (s.kind !== "session") return;
      await chats.persistSnapshot(s.account.accountId);
    },
    ownProfileEnvelope: () => profiles.ownProfileEnvelope(),
    onUsernameResolved: (key, username) => profiles.learnUsername(key, username),
  });

  const [accepted, setAccepted] = createSignal<AcceptedContact[]>(loadAccepted());
  onMount(() => {
    const unsubscribe = subscribeAcceptedChanged(() => setAccepted(loadAccepted()));
    onCleanup(unsubscribe);
  });
  const [name, setName] = createSignal("");
  const [picked, setPicked] = createSignal<ReadonlySet<string>>(new Set());
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createLiveText();

  // A contact can be added without a username lookup only when its account id
  // was captured at accept (ADR-0021); older records lack it.
  const addable = (c: AcceptedContact): boolean => c.accountIdHex !== undefined;
  const toggle = (key: string): void => {
    const next = new Set(picked());
    if (next.has(key)) next.delete(key);
    else next.add(key);
    setPicked(next);
  };
  const labelOf = (c: AcceptedContact): string => profiles.resolve(c.signaturePubkeyB64).name;

  async function onCreate(e: SubmitEvent): Promise<void> {
    e.preventDefault();
    setError(null);
    const secrets = auth.identitySecrets();
    if (secrets === null) {
      setError(() => t("new_group.error.sign_in"));
      return;
    }
    const picks = accepted().filter((c) => picked().has(c.signaturePubkeyB64) && addable(c));
    const [first, ...rest] = picks;
    if (first === undefined) return;
    setBusy(true);
    try {
      const result = await client.startGroupWith(
        secrets,
        accountIdBytesFromHex(first.accountIdHex as string),
        base64Decode(first.signaturePubkeyB64),
      );
      const active = chats.add({
        label: name().trim().length > 0 ? name().trim() : "New group",
        group: result.group,
      });
      profiles.groupJoined(active.groupIdB64);
      // ADR-0051 §3 — the name is a shared fact: into the device store and,
      // right after the creating commit, into the group so the first Welcome
      // recipient gets it with its first poll. A failed send is covered by
      // the growth re-send. The group exists at this point whatever happens
      // here, so a refusal is logged rather than shown on the create form.
      if (name().trim().length > 0) {
        try {
          await groupNames.recordCreated(active, name());
        } catch (err) {
          // eslint-disable-next-line no-console
          console.warn("new group: name not recorded", err);
        }
      }
      const s = auth.state();
      if (s.kind === "session") {
        // docs/ux/client-redesign.md section 3 — a group created here is a
        // group even while it still has two people.
        recordConversationKind(s.account.accountId, active.groupIdB64, "group");
        await chats.persistSnapshot(s.account.accountId);
      }
      if (rest.length > 0) {
        setPendingGroupSetup({
          groupIdB64: active.groupIdB64,
          members: rest.map((c) => ({
            accountIdHex: c.accountIdHex as string,
            signaturePubkeyB64: c.signaturePubkeyB64,
            label: labelOf(c),
          })),
        });
      }
      navigate(paths.groupChat(base64UrlEncodeNoPad(result.group.groupId)), { replace: true });
    } catch (err) {
      setError(contactsErrorText(err, "chats.new_group.error"));
      setBusy(false);
    }
  }

  return (
    <article>
      <form class="lk-page lk-stack" style={{ "max-width": "560px" }} onSubmit={(e) => void onCreate(e)}>
        <button
          type="button"
          class="ghost small lk-narrow-only"
          style={{ "align-self": "flex-start" }}
          onClick={() => navigate(paths.groups)}
        >
          <Icon name="back" size={16} />
          {t("new_group.back")}
        </button>
        <div class="lk-page-head">
          <h1>{t("chats.new_group.title")}</h1>
          <p>{t("new_group.subtitle")}</p>
        </div>
        <section class="lk-card">
          <label>
            {t("chat.group_name.label")}
            <input
              type="text"
              value={name()}
              maxLength={GROUP_NAME_MAX}
              placeholder={t("new_group.name_placeholder")}
              disabled={busy()}
              onInput={(e) => setName(e.currentTarget.value)}
            />
          </label>
          <p class="lk-card-desc" style={{ margin: 0 }}>
            {t("new_group.name_hint")}
          </p>
        </section>
        <section class="lk-card" style={{ gap: "4px" }}>
          <h2>
            {t("chat.members.title")}
            <Show when={picked().size > 0}>
              <span class="form-info" style={{ "font-weight": 400, "margin-left": "8px" }}>
                {t("chat.select.count", { count: picked().size })}
              </span>
            </Show>
          </h2>
          <Show
            when={accepted().length > 0}
            fallback={
              <div class="lk-empty" style={{ padding: "24px" }}>
                <span>{t("new_group.no_contacts")}</span>
                <button type="button" onClick={() => navigate(paths.contacts)}>
                  {t("chats.new_group.open_contacts")}
                </button>
              </div>
            }
          >
            <For each={accepted()}>
              {(c) => (
                <label class="lk-row pick" style={{ opacity: addable(c) ? 1 : 0.55 }}>
                  <input
                    type="checkbox"
                    checked={picked().has(c.signaturePubkeyB64)}
                    disabled={busy() || !addable(c)}
                    onChange={() => toggle(c.signaturePubkeyB64)}
                  />
                  <Avatar label={labelOf(c)} id={c.signaturePubkeyB64} />
                  <div class="lk-row-body">
                    <span class="lk-row-title">{labelOf(c)}</span>
                    <Show when={!addable(c)}>
                      <span class="lk-row-sub">{t("chats.new_group.accept_again")}</span>
                    </Show>
                  </div>
                </label>
              )}
            </For>
          </Show>
        </section>
        <Show when={error()}>{(m) => <div class="form-error">{m()}</div>}</Show>
        <div class="lk-actions">
          <button type="submit" class="primary" disabled={busy() || picked().size === 0}>
            {busy() ? t("chats.new_group.creating") : t("chats.create.group")}
          </button>
          <button type="button" class="ghost" disabled={busy()} onClick={() => navigate(paths.groups)}>
            {t("common.cancel")}
          </button>
        </div>
      </form>
    </article>
  );
}
