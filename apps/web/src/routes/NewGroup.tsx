// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `/groups/new` — name a group and pick its first members from your contacts
 * (UX spec `docs/ux/new-group.md`). Creates the group with the first pick and
 * hands the rest to the chat view, which adds them through its own add path.
 * A group may start with nobody else: a name is enough, and people are added
 * later as in any group.
 */
import { createEffect, createSignal, For, onCleanup, onMount, Show, type JSX } from "solid-js";
import { DiscoveryNetworkError } from "../discovery/client";
import { anonTokenMessage } from "../tokens/token-copy";
import { HandleWriteError } from "../discovery/handle-lookup";
import { HANDLE_CHECK_DEBOUNCE_MS, handleTaken as askHandleTaken, isRateLimited } from "../discovery/handle-check";
import {
  claimGroupHandle,
  handleRandom,
  handleWellFormed,
  suggestedHandle,
  type PublicGroupFacts,
} from "../chat/group-handle";
import type { StoredConversationHandle } from "../chat/group-settings-envelope";
import { handleWriteErrorText } from "../chat/GroupInfoExtras";
import { knockKeyFor } from "../contacts/client";
import { LakkoPicker } from "../ui/LakkoPicker";
import { AvatarCamera, AvatarCrop, AvatarMenu, type CropSource } from "../profile/AvatarEditor";
import { releaseLocalAvatar, type LocalAvatar } from "../profile/avatar-media";
import { uploadLocalAvatar } from "../profile/avatar-upload";
import type { AvatarData } from "../profile/avatar-model";
import { loadWasm } from "../wasm";
import { useLocation, useNavigate } from "@solidjs/router";

import { useAuth } from "../auth/AuthContext";
import { base64UrlEncodeNoPad } from "../auth/account-id";
import { useChats } from "../chat/ChatContext";
import { useGroupNames } from "../chat/GroupNameContext";
import { setPendingGroupSetup } from "../chat/pending-group-setup";
import { ContactsClient } from "../contacts/client";
import {
  loadAccepted,
  subscribeAcceptedChanged,
  type AcceptedContact,
} from "../contacts/local-store";
import { useProfiles } from "../profile/ProfileContext";
import { Avatar } from "../ui/Avatar";
import { Icon } from "../ui/icons";
import { t } from "../l10n";
import { DEFAULT_GROUP_LABEL } from "../chat/group-title";
import { createLiveText } from "../l10n/live-text";
import { paths } from "./paths";
import { screenAnchor } from "../ui/screen-anchor";
import { durableSnapshotHook, sessionAccountOf } from "../chat/persist-hooks";
import { markGroupCreated } from "../chat/created-groups";

export const GROUP_NAME_MAX = 64;

export default function NewGroup(): JSX.Element {
  const auth = useAuth();
  const chats = useChats();
  const profiles = useProfiles();
  const groupNames = useGroupNames();
  const navigate = useNavigate();
  const location = useLocation();

  const client = new ContactsClient(undefined, undefined, {
    identityProvider: () => chats.identity(),
    onPersist: durableSnapshotHook(chats, () => sessionAccountOf(auth.state())),
    ownProfileEnvelope: () => profiles.ownProfileEnvelope(),
    onUsernameResolved: (key, username) => profiles.learnUsername(key, username),
  });

  const [accepted, setAccepted] = createSignal<AcceptedContact[]>(loadAccepted());
  onMount(() => {
    const unsubscribe = subscribeAcceptedChanged(() => setAccepted(loadAccepted()));
    onCleanup(unsubscribe);
  });
  const [name, setName] = createSignal("");
  // ADR-0087 / UX 3.1 — the @name, proposed from the name until edited.
  const [isPublic, setIsPublic] = createSignal(false);
  const [handle, setHandle] = createSignal("");
  const [handleEdited, setHandleEdited] = createSignal(false);
  const [handleTaken, setHandleTaken] = createSignal(false);
  const [handleError, setHandleError] = createSignal<string | null>(null);
  const random = handleRandom();
  let wasmNow: Awaited<ReturnType<typeof loadWasm>> | null = null;
  void loadWasm().then((w) => {
    wasmNow = w;
    if (!handleEdited()) setHandle(suggestedHandle(w, name(), !isPublic(), random));
  });
  createEffect(() => {
    const n = name();
    const priv = !isPublic();
    if (handleEdited() || wasmNow === null) return;
    setHandle(suggestedHandle(wasmNow, n, priv, random));
  });
  const handleWell = (): boolean => wasmNow === null || handleWellFormed(wasmNow, handle());
  // UX 3.3 — public: a name that answers a lookup reads "taken", 500 ms
  // after typing stops, from 3 characters. Private: nothing.
  let takenTimer: ReturnType<typeof setTimeout> | null = null;
  createEffect(() => {
    const h = handle();
    const pub = isPublic();
    setHandleTaken(false);
    if (takenTimer !== null) clearTimeout(takenTimer);
    if (!pub || h.length < 3 || !handleWell()) return;
    // Asked as seldom as search asks (who-can-find-me 6.1): once typing
    // stops, never twice for one name within 60 s; Create reuses the name's
    // OPRF answer (`discovery/handle-check.ts`).
    takenTimer = setTimeout(() => {
      void askHandleTaken(h).then((taken) => {
        if (handle() === h && taken !== null) setHandleTaken(taken);
      });
    }, HANDLE_CHECK_DEBOUNCE_MS);
  });
  onCleanup(() => {
    if (takenTimer !== null) clearTimeout(takenTimer);
  });
  // UX 4.5.3 — the optional photo, uploaded when Create is pressed.
  const [photo, setPhoto] = createSignal<LocalAvatar | null>(null);
  const [photoMenu, setPhotoMenu] = createSignal(false);
  const [cropSource, setCropSource] = createSignal<CropSource | null>(null);
  const [cameraOpen, setCameraOpen] = createSignal(false);
  const [photoError, setPhotoError] = createSignal<string | null>(null);
  onCleanup(() => releaseLocalAvatar(photo()));
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
  const usernameLineOf = (c: AcceptedContact): string | null => {
    const username = profiles.resolve(c.signaturePubkeyB64).username;
    return username !== null && username !== labelOf(c) ? username : null;
  };

  /** UX 3.1 — Create is enabled when the @name is well formed and, if Public,
   *  there is a name. Nobody has to be picked. */
  const mayCreate = (): boolean =>
    handleWell() && handle().length >= 3 && (!isPublic() || name().trim().length > 0) && !busy();

  async function onCreate(e: SubmitEvent): Promise<void> {
    e.preventDefault();
    setError(null);
    const secrets = auth.identitySecrets();
    if (secrets === null) {
      setError(() => t("new_group.error.sign_in"));
      return;
    }
    const picks = accepted().filter((c) => picked().has(c.signaturePubkeyB64) && addable(c));
    if (!mayCreate()) return;
    setBusy(true);
    setHandleError(null);
    setPhotoError(null);
    // The group is created and recorded whether or not the person waits for
    // it; only opening it is addressed to this screen (`ui/screen-anchor.ts`).
    const here = screenAnchor(location);
    try {
      const wasm = await loadWasm();
      // UX 4.5.3 — the photo first: a failed upload creates nothing.
      let avatar: AvatarData | null = null;
      const local = photo();
      if (local !== null) {
        try {
          avatar = await uploadLocalAvatar(local);
        } catch (err) {
          const text = err instanceof Error ? err.message : "";
          setPhotoError(/limit|429/i.test(text) ? t("group_photo.error.limit") : t("group_photo.error.upload"));
          setBusy(false);
          return;
        }
      }
      const account = chats.accountIdentity();
      if (account === null) throw new Error("no account identity");
      let claimed: StoredConversationHandle | null = null;
      // ADR-0087 sections 3 and 10 — the group starts with its creator; the
      // @name is claimed before anything about the group leaves this device,
      // so a held name creates nothing. Picks are added through the normal
      // add path.
      const result = await client.startGroupAlone(secrets, async (group) => {
        let facts: PublicGroupFacts | null = null;
        if (isPublic()) {
          const knock = await knockKeyFor(wasm, account, group, chats.conversationAccountIdentity());
          if (knock === null) throw new Error("no knock key");
          facts = {
            name: name().trim(),
            memberCount: 1 + picks.length,
            knockPublicKey: knock.publicKey,
      knockVersion: knock.version,
            avatar,
          };
        }
        claimed = await claimGroupHandle({
          wasm,
          accountIdentity: account,
          groupId: group.groupId,
          handle: handle(),
          isPublic: isPublic(),
          facts,
        });
      });
      const active = chats.add({
        label: name().trim().length > 0 ? name().trim() : DEFAULT_GROUP_LABEL,
        group: result.group,
      });
      profiles.groupJoined(active.groupIdB64);
      // Section 10 — the timeline row "You created this group".
      {
        const acct = sessionAccountOf(auth.state());
        if (acct !== null) void markGroupCreated(acct, active.groupIdB64).catch(() => undefined);
      }
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
      if (claimed !== null) await groupNames.setHandle(active, claimed);
      if (avatar !== null) {
        try {
          await groupNames.setPhoto(active, avatar);
        } catch (err) {
          // Kept; the growth re-send carries it.
          // eslint-disable-next-line no-console
          console.warn("new group: photo not sent", err instanceof Error ? err.name : "");
        }
      }
      const rest = picks;
      const s = auth.state();
      if (s.kind === "session") {
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
      if (here()) {
        navigate(paths.groupChat(base64UrlEncodeNoPad(result.group.groupId)), { replace: true });
      }
    } catch (err) {
      if (isRateLimited(err) || anonTokenMessage(err, "") !== null) {
        // A refused lookup is the rate, never a refusal of the name.
        setError(() => handleWriteErrorText(err));
      } else if (err instanceof HandleWriteError || err instanceof DiscoveryNetworkError) {
        const text = handleWriteErrorText(err);
        if (err instanceof HandleWriteError && err.code === "name-taken") setHandleError(text);
        else setError(() => text);
      } else {
        setError(() => t("new_group.error.secure_setup"));
      }
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
        <section class="lk-card lk-info-header">
          <div class="lk-profile-avatar lk-menu-anchor">
            <Avatar
              label={name().trim().length > 0 ? name() : "#"}
              id={handle()}
              size="profile"
              media={photo() === null ? null : { stillUrl: photo()?.stillUrl ?? null, videoUrl: photo()?.videoUrl ?? null }}
              playVideo
            />
            <button
              type="button"
              class="lk-icon-button lk-profile-avatar-edit"
              title={t("profile.avatar.change")}
              aria-label={t("profile.avatar.change")}
              aria-haspopup="menu"
              aria-expanded={photoMenu()}
              disabled={busy()}
              onClick={() => setPhotoMenu((v) => !v)}
            >
              <Icon name="pencil" size={16} />
            </button>
            <Show when={photoMenu()}>
              <AvatarMenu
                canRemove={photo() !== null}
                onClose={() => setPhotoMenu(false)}
                onPick={(source) => {
                  setPhotoMenu(false);
                  setCropSource(source);
                }}
                onCamera={() => {
                  setPhotoMenu(false);
                  setCameraOpen(true);
                }}
                onRemove={() => {
                  setPhotoMenu(false);
                  releaseLocalAvatar(photo());
                  setPhoto(null);
                  setPhotoError(null);
                }}
                onUnusable={() => {
                  setPhotoMenu(false);
                  setPhotoError(t("profile.video.unusable"));
                }}
              />
            </Show>
          </div>
          <Show when={photoError()}>{(m) => <p class="form-error" role="alert">{m()}</p>}</Show>
        </section>
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
          <Show when={isPublic() && name().trim().length === 0}>
            <p class="form-error" style={{ margin: 0 }}>
              {t("group_type.needs_name")}
            </p>
          </Show>
          <label>
            {t("group_type.handle")}
            <input
              type="text"
              dir="ltr"
              value={`@${handle()}`}
              maxLength={33}
              autocomplete="off"
              disabled={busy()}
              aria-invalid={!handleWell() || handleError() !== null}
              onInput={(e) => {
                setHandleEdited(true);
                setHandleError(null);
                setHandle(e.currentTarget.value.replace(/^@/, "").trim());
              }}
            />
          </label>
          <Show
            when={handleWell()}
            fallback={<p class="form-error" style={{ margin: 0 }}>{t("group_handle.error.bad_form")}</p>}
          >
            <Show
              when={handleError()}
              fallback={
                <Show
                  when={handleTaken()}
                  fallback={
                    <p class="lk-card-desc" style={{ margin: 0 }}>
                      {isPublic() ? t("group_type.handle_public") : t("group_type.handle_private")}
                    </p>
                  }
                >
                  <p class="form-error" style={{ margin: 0 }}>
                    {t("group_handle.error.taken")}
                  </p>
                </Show>
              }
            >
              {(m) => <p class="form-error" style={{ margin: 0 }}>{m()}</p>}
            </Show>
          </Show>
          <label id="lk-new-group-who">{t("group_type.who_can_join")}</label>
          <LakkoPicker
            value={isPublic() ? "public" : "private"}
            labelledBy="lk-new-group-who"
            options={[
              { value: "private", label: t("group_type.private") },
              { value: "public", label: t("group_type.public") },
            ]}
            disabled={busy()}
            onChange={(v) => setIsPublic(v === "public")}
          />
          <p class="lk-card-desc" style={{ margin: 0 }}>
            {isPublic() ? t("group_type.public_desc") : t("group_type.private_desc")}
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
                  <Avatar label={labelOf(c)} id={c.signaturePubkeyB64} personKey={c.signaturePubkeyB64} />
                  <div class="lk-row-body">
                    <span class="lk-row-title">{labelOf(c)}</span>
                    <Show
                      when={addable(c)}
                      fallback={<span class="lk-row-sub">{t("chats.new_group.accept_again")}</span>}
                    >
                      {/* The username as Add members, iOS and Android show it; not
                          when the title already is the username. */}
                      <Show when={usernameLineOf(c)}>{(u) => <span class="lk-row-sub">{u()}</span>}</Show>
                    </Show>
                  </div>
                </label>
              )}
            </For>
          </Show>
        </section>
        <Show when={error()}>{(m) => <div class="form-error">{m()}</div>}</Show>
        <div class="lk-actions">
          <button type="submit" class="primary" disabled={!mayCreate()}>
            {busy() ? t("chats.new_group.creating") : t("chats.create.group")}
          </button>
          <button type="button" class="ghost" disabled={busy()} onClick={() => navigate(paths.groups)}>
            {t("common.cancel")}
          </button>
        </div>
      </form>
      <Show when={cropSource()}>
        {(source) => (
          <AvatarCrop
            source={source()}
            onCancel={() => setCropSource(null)}
            onUse={(local) => {
              setCropSource(null);
              releaseLocalAvatar(photo());
              setPhoto(local);
              setPhotoError(null);
            }}
          />
        )}
      </Show>
      <Show when={cameraOpen()}>
        <AvatarCamera
          onClose={() => setCameraOpen(false)}
          onCaptured={(source) => {
            setCameraOpen(false);
            setCropSource(source);
          }}
        />
      </Show>
    </article>
  );
}
