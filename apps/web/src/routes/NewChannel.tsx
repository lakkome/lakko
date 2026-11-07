// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `/channel/new` — the New channel form (`docs/ux/group-and-channel-privacy.md`
 * sections 3.2, 3.3 and 4.5.3): the photo, `Channel name`, `Description`, the
 * `@name` proposed from the name, `Who can subscribe`, and while Public the
 * `Topic` and `Language`. Opens beside the Chats list; Create opens the
 * channel where any conversation opens.
 *
 * The form holds no channel machinery: `createChannelFull` uploads the photo,
 * claims the `@name`, founds the channel and records it, and throws a
 * `ChannelError` code the form maps to its line (`channel-error-copy.ts`).
 */
import { createEffect, createSignal, onCleanup, Show, type JSX } from "solid-js";
import { useLocation, useNavigate } from "@solidjs/router";

import { useAuth } from "../auth/AuthContext";
import { useChats } from "../chat/ChatContext";
import { sessionAccountOf } from "../chat/persist-hooks";
import { handleRandom, handleWellFormed } from "../chat/group-handle";
import { createChannelFull, type ChannelVisibility } from "../channels/channel-access";
import { channelErrorCode, channelErrorPlace, channelErrorText } from "../channels/channel-error-copy";
import { HANDLE_CHECK_DEBOUNCE_MS, handleTaken as askHandleTaken } from "../discovery/handle-check";
import { appLanguage, languageLabel, languageOptions } from "../discover/languages";
import { topicLabel } from "../discover/topic-names";
import { AvatarCamera, AvatarCrop, AvatarMenu, type CropSource } from "../profile/AvatarEditor";
import { releaseLocalAvatar, type LocalAvatar } from "../profile/avatar-media";
import { currentLanguage, t } from "../l10n";
import { createLiveText } from "../l10n/live-text";
import { LakkoPicker } from "../ui/LakkoPicker";
import { Avatar } from "../ui/Avatar";
import { Icon } from "../ui/icons";
import { screenAnchor } from "../ui/screen-anchor";
import { loadWasm } from "../wasm";
import { paths } from "./paths";

/** The longest channel name the metadata record takes, in characters. */
export const CHANNEL_NAME_MAX = 64;

/** UX 2.1 — the proposed `@name` of a channel. */
function suggestChannelHandle(
  wasm: Awaited<ReturnType<typeof loadWasm>>,
  name: string,
  isPrivate: boolean,
  random: bigint,
): string {
  try {
    return wasm.suggestHandle(name, "channel", isPrivate, random);
  } catch {
    return "";
  }
}

/** The topic ids the core knows, in its order. */
function topicIds(wasm: Awaited<ReturnType<typeof loadWasm>> | null): number[] {
  if (wasm === null) return [0];
  try {
    return Array.from(wasm.channelTopicIds());
  } catch {
    return [0];
  }
}

export default function NewChannel(): JSX.Element {
  const auth = useAuth();
  const chats = useChats();
  const navigate = useNavigate();
  const location = useLocation();

  const [wasm, setWasm] = createSignal<Awaited<ReturnType<typeof loadWasm>> | null>(null);
  void loadWasm().then(setWasm);

  const [name, setName] = createSignal("");
  const [description, setDescription] = createSignal("");
  const [visibility, setVisibility] = createSignal<ChannelVisibility>("private");
  const isPublic = (): boolean => visibility() === "public";
  const [topic, setTopic] = createSignal(0);
  const [language, setLanguage] = createSignal(appLanguage(currentLanguage() ?? navigator.language));

  // UX 2.1 — the @name follows the name until the person edits it.
  const random = handleRandom();
  const [handle, setHandle] = createSignal("");
  const [handleEdited, setHandleEdited] = createSignal(false);
  createEffect(() => {
    const w = wasm();
    const n = name();
    const priv = !isPublic();
    if (w === null || handleEdited()) return;
    setHandle(suggestChannelHandle(w, n, priv, random));
  });
  const handleWell = (): boolean => {
    const w = wasm();
    return w === null || handleWellFormed(w, handle());
  };

  // UX 3.3 — public only: a name that answers a lookup reads "taken", 500 ms
  // after typing stops, from 3 characters. Private: nothing until Create.
  const [handleTaken, setHandleTaken] = createSignal(false);
  const [handleError, setHandleError] = createSignal<string | null>(null);
  let takenTimer: ReturnType<typeof setTimeout> | null = null;
  createEffect(() => {
    const h = handle();
    const pub = isPublic();
    setHandleTaken(false);
    if (takenTimer !== null) clearTimeout(takenTimer);
    if (!pub || h.length < 3 || !handleWell()) return;
    takenTimer = setTimeout(() => {
      void askHandleTaken(h).then((taken) => {
        if (handle() === h && isPublic() && taken !== null) setHandleTaken(taken);
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

  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createLiveText();

  /** UX 3.2: the channel name is required; the @name must be well formed. */
  const mayCreate = (): boolean =>
    !busy() && name().trim().length > 0 && handle().length >= 3 && handleWell();

  async function onCreate(e: SubmitEvent): Promise<void> {
    e.preventDefault();
    if (!mayCreate()) return;
    setError(null);
    setHandleError(null);
    setPhotoError(null);
    const identity = chats.conversationAccountIdentity();
    const current = chats.accountIdentity();
    if (auth.identitySecrets() === null || identity === null || current === null) {
      setError(() => t("channels.new.error.sign_in"));
      return;
    }
    setBusy(true);
    // The channel exists whether or not the person waits; opening it is for
    // a person still on this screen (`ui/screen-anchor.ts`).
    const here = screenAnchor(location);
    try {
      const id = await createChannelFull(identity, current, sessionAccountOf(auth.state()), {
        name: name().trim(),
        description: description().trim(),
        handle: handle(),
        visibility: visibility(),
        topic: topic(),
        language: language(),
        photo: photo(),
      });
      if (here()) navigate(paths.channel(id), { replace: true });
    } catch (err) {
      const code = channelErrorCode(err);
      const text = channelErrorText(err, () => t("channels.new.failed"));
      switch (channelErrorPlace(code)) {
        case "handle":
          setHandleError(text);
          break;
        case "photo":
          setPhotoError(text);
          break;
        case "form":
          if (code === null) console.warn("[channels] create failed", err instanceof Error ? err.name : "");
          setError(() => text);
          break;
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
          onClick={() => navigate(paths.home)}
        >
          <Icon name="back" size={16} />
          {t("nav.chats")}
        </button>
        <div class="lk-page-head">
          <h1>{t("channels.new.title")}</h1>
          <p>{t("channels.new.subtitle")}</p>
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
            {t("channels.new.name_label")}
            <input
              type="text"
              required
              value={name()}
              maxLength={CHANNEL_NAME_MAX}
              disabled={busy()}
              onInput={(e) => setName(e.currentTarget.value)}
            />
          </label>
          <label>
            {t("channels.new.description_label")}
            <textarea
              rows={3}
              value={description()}
              disabled={busy()}
              onInput={(e) => setDescription(e.currentTarget.value)}
            />
          </label>
          <label>
            {t("group_type.handle")}
            <input
              type="text"
              dir="ltr"
              value={`@${handle()}`}
              maxLength={33}
              autocomplete="off"
              autocapitalize="none"
              spellcheck={false}
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
                      {isPublic() ? t("channel_type.handle_public") : t("channel_type.handle_private")}
                    </p>
                  }
                >
                  <p class="form-error" style={{ margin: 0 }}>{t("group_handle.error.taken")}</p>
                </Show>
              }
            >
              {(m) => <p class="form-error" style={{ margin: 0 }}>{m()}</p>}
            </Show>
          </Show>
          <label id="lk-new-channel-who">{t("channel_type.who_can_subscribe")}</label>
          <LakkoPicker
            value={visibility()}
            labelledBy="lk-new-channel-who"
            options={[
              { value: "private", label: t("group_type.private") },
              { value: "public", label: t("group_type.public") },
            ]}
            disabled={busy()}
            onChange={(v) => setVisibility(v === "public" ? "public" : "private")}
          />
          <p class="lk-card-desc" style={{ margin: 0 }}>
            {isPublic() ? t("channel_type.public_desc") : t("channel_type.private_desc")}
          </p>
          <Show when={isPublic()}>
            <label id="lk-new-channel-topic">{t("discover.listing.topic")}</label>
            <LakkoPicker
              value={String(topic())}
              labelledBy="lk-new-channel-topic"
              options={topicIds(wasm()).map((id) => ({ value: String(id), label: topicLabel(id) }))}
              disabled={busy()}
              onChange={(v) => setTopic(Number.parseInt(v, 10) || 0)}
            />
            <label id="lk-new-channel-language">{t("discover.language")}</label>
            <LakkoPicker
              value={language()}
              labelledBy="lk-new-channel-language"
              options={languageOptions(language()).map((code) => ({ value: code, label: languageLabel(code) }))}
              disabled={busy()}
              onChange={(v) => setLanguage(v)}
            />
          </Show>
        </section>
        <Show when={error()}>
          {(m) => (
            <div class="form-error" role="alert">
              {m()}
            </div>
          )}
        </Show>
        <div class="lk-actions">
          <button type="submit" class="primary" disabled={!mayCreate()}>
            {busy() ? t("chats.new_group.creating") : t("channels.new.create")}
          </button>
          <button type="button" class="ghost" disabled={busy()} onClick={() => navigate(paths.home)}>
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
