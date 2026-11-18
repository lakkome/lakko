// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The info screen of a channel (UX `docs/ux/group-and-channel-privacy.md`
 * sections 4, 4.5.2 and 12.1; ADR-0087 section 15). The same order as a
 * group's: the header (photo, name, `@name`, the line saying what it is,
 * the description), the actions and detail, the exit block.
 *
 * Which blocks appear is the shared rule (`lakko_ui_rules::channel_info_sections`
 * through `chat/conversation-info.ts`); this file draws what it is told. The
 * rows open pushed screens (`ChannelInfoScreens.tsx`).
 */
import { createMemo, createResource, createSignal, Match, Show, Switch, type JSX } from "solid-js";

import { channelInfoSections, type InfoSection } from "../chat/conversation-info";
import { copyMessageText } from "../chat/copy-message";
import { useChats } from "../chat/ChatContext";
import { useMembershipEnd } from "../chat/MembershipEndContext";
import { uiRules } from "../chat/ui-rules";
import { AvatarCamera, AvatarCrop, AvatarMenu, type CropSource } from "../profile/AvatarEditor";
import { releaseLocalAvatar, type LocalAvatar } from "../profile/avatar-media";
import { MembersCanFindMeSwitch } from "../settings/MembersCanFindMeSwitch";
import { t } from "../l10n";
import { Avatar } from "../ui/Avatar";
import { requestConfirm } from "../ui/confirm-dialog";
import { Icon } from "../ui/icons";
import { InfoRowButton } from "../ui/LabelledRows";
import { PushedScreen } from "../ui/PushedScreen";
import { channelAvatarKey } from "./channel-avatar";
import { readerDeviceSections } from "./channel-subscription-plan";
import {
  channelAvatarOf,
  channelHandleOf,
  channelReaderDeviceRoleOf,
  channelRoleOf,
  channelSubscriberCount,
  channelVisibilityOf,
  deleteChannel,
  saveChannelName,
  saveChannelPhoto,
  saveChannelReactions,
} from "./channel-access";
import { AllowedReactionsScreen, allowedSummary } from "../chat/GroupInfoExtras";
import { channelAllowedReactions } from "./PostReactions";
import { channelErrorText } from "./channel-error-copy";
import { channelMetaErrorCode, channelMetaErrorText } from "./channel-meta";
import { channelPublishers } from "./channel-publishers";
import { channelFacts, channelHeaderLine } from "./channel-facts";
import {
  ChannelInvitePeopleScreen,
  ChannelInviteLinksScreen,
  ChannelPublishersScreen,
  ChannelSubscribersScreen,
  ChannelTypeScreen,
} from "./ChannelInfoScreens";

export type ChannelInfoScreen = "type" | "subscribers" | "publishers" | "invite-people" | "invite-link" | "reactions";
type Screen = ChannelInfoScreen;

export function ChannelInfo(props: {
  readonly id: string;
  /** The person unsubscribed: the conversation goes. */
  readonly onEnded: () => void;
  /** Open a screen pushed from the info. The conversation view draws it over
   *  the whole pane (`ChannelInfoScreenHost`), as a group's info pushes its
   *  screens, so it never stacks under the info's own header. */
  readonly onOpenScreen: (screen: ChannelInfoScreen) => void;
}): JSX.Element {
  const chats = useChats();
  const membershipEnd = useMembershipEnd();
  const role = (): ReturnType<typeof channelRoleOf> => channelRoleOf(props.id);
  const isOwner = (): boolean => role() === "owner";
  const publishes = (): boolean => role() === "owner" || role() === "publisher";
  const facts = (): ReturnType<typeof channelFacts> => channelFacts(props.id);
  const handle = (): string | null => channelHandleOf(props.id);

  const sections = createMemo((): readonly InfoSection[] => {
    const r = role();
    if (r === null) return [];
    // 2026-11-13 (UX second-device.md section 4a): on a device of the owner
    // or a publisher that cannot post here, a subscriber's blocks and an empty
    // exit block.
    return readerDeviceSections(
      channelInfoSections(uiRules(), {
        role: r,
        hasHandle: handle() !== null,
        hasOtherPublishers: channelPublishers(props.id).some((p) => !p.isOwner),
        // Shown for every role: the switch itself reads the setting and says
        // when it is off (`settings/MembersCanFindMeSwitch.tsx`).
        membersOfGroupsAndChannelsOn: true,
      }),
      channelReaderDeviceRoleOf(props.id),
    );
  });
  const has = (s: InfoSection): boolean => sections().includes(s);

  // The exact count, to the owner and the publishers (UX 15.1).
  const [count] = createResource(
    () => (publishes() ? props.id : null),
    async (id) => {
      const identity = chats.conversationAccountIdentity();
      if (identity === null) return null;
      try {
        return await channelSubscriberCount(identity, id);
      } catch {
        return null;
      }
    },
  );

  const setScreen = (s: Screen): void => props.onOpenScreen(s);

  // ----- the name and description, edited in place by the owner -----
  const [editing, setEditing] = createSignal(false);
  const [draftName, setDraftName] = createSignal("");
  const [draftDescription, setDraftDescription] = createSignal("");
  const [saving, setSaving] = createSignal(false);
  const [nameError, setNameError] = createSignal<string | null>(null);

  async function saveName(): Promise<void> {
    const identity = chats.conversationAccountIdentity();
    if (identity === null || saving()) return;
    if (draftName().trim().length === 0) {
      setNameError(t("channels.meta.error.name_empty"));
      return;
    }
    setSaving(true);
    setNameError(null);
    try {
      await saveChannelName(identity, props.id, draftName().trim(), draftDescription().trim());
      setEditing(false);
    } catch (err) {
      // A name or description the record refuses is worded by its code.
      const metaCode = channelMetaErrorCode(err);
      setNameError(
        metaCode !== undefined
          ? channelMetaErrorText(metaCode)
          : channelErrorText(err, () => t("channels.meta.error.save")),
      );
    } finally {
      setSaving(false);
    }
  }

  // ----- the photo (UX 4.5.2): the owner only, no Save -----
  const [menuOpen, setMenuOpen] = createSignal(false);
  const [cropSource, setCropSource] = createSignal<CropSource | null>(null);
  const [cameraOpen, setCameraOpen] = createSignal(false);
  const [uploading, setUploading] = createSignal<LocalAvatar | null>(null);
  const [lastLocal, setLastLocal] = createSignal<LocalAvatar | null>(null);
  const [photoError, setPhotoError] = createSignal<string | null>(null);

  async function usePhoto(local: LocalAvatar): Promise<void> {
    const identity = chats.conversationAccountIdentity();
    if (identity === null) return;
    setPhotoError(null);
    setUploading(local);
    setLastLocal(local);
    try {
      if (!isOwner()) {
        setPhotoError(t("channels.photo.only_owner"));
        return;
      }
      await saveChannelPhoto(identity, props.id, local);
      releaseLocalAvatar(local);
      setLastLocal(null);
    } catch (err) {
      setPhotoError(channelErrorText(err, () => t("group_photo.error.upload")));
    } finally {
      setUploading(null);
    }
  }

  async function removePhoto(): Promise<void> {
    const ok = await requestConfirm({
      title: t("channels.photo.remove_title"),
      body: t("channels.photo.remove_body"),
      confirmLabel: t("group_photo.remove.confirm"),
      cancelLabel: t("common.cancel"),
      tone: "danger",
    });
    if (!ok) return;
    const identity = chats.conversationAccountIdentity();
    if (identity === null) return;
    setPhotoError(null);
    try {
      await saveChannelPhoto(identity, props.id, null);
    } catch (err) {
      setPhotoError(channelErrorText(err, () => t("group_photo.error.upload")));
    }
  }

  const [handleCopy, setHandleCopy] = createSignal<"idle" | "copied" | "failed">("idle");

  async function unsubscribe(): Promise<void> {
    const name = facts().name;
    const ok = await requestConfirm({
      title:
        name.trim().length > 0
          ? t("channels.unsubscribe.title_named", { name })
          : t("channels.unsubscribe.title"),
      body: t("channels.unsubscribe.body"),
      confirmLabel: t("channels.unsubscribe.action"),
      cancelLabel: t("common.cancel"),
      tone: "danger",
    });
    if (!ok) return;
    await membershipEnd.endChannel(props.id, name);
    props.onEnded();
  }

  // UX 11 / 12.1 — the owner deletes a channel no publisher remains on.
  const [deleteError, setDeleteError] = createSignal<string | null>(null);
  async function removeChannel(): Promise<void> {
    const h = handle();
    const ok = await requestConfirm({
      title: t("channels.delete.title"),
      body: h === null ? t("channels.delete.body_no_handle") : t("channels.delete.body", { name: h }),
      confirmLabel: t("common.delete"),
      cancelLabel: t("common.cancel"),
      tone: "danger",
    });
    if (!ok) return;
    const lineage = chats.conversationAccountIdentity();
    const current = chats.accountIdentity();
    if (lineage === null || current === null) return;
    setDeleteError(null);
    try {
      await deleteChannel(lineage, current, props.id);
    } catch (err) {
      // Nothing changed: the servers were not reached, or not the owner.
      setDeleteError(channelErrorText(err, () => t("group_handle.error.offline")));
      return;
    }
    // The row goes with no banner: the person asked for it.
    await membershipEnd.endChannel(props.id, facts().name);
    props.onEnded();
  }

  return (
    <>
      <section class="lk-card lk-info-header">
        <div class="lk-profile-avatar lk-menu-anchor" classList={{ uploading: uploading() !== null }}>
          <Avatar
            label={facts().name}
            id={props.id}
            size="profile"
            conversationKey={channelAvatarKey(props.id)}
            media={
              uploading() !== null
                ? { stillUrl: uploading()?.stillUrl ?? null, videoUrl: uploading()?.videoUrl ?? null }
                : undefined
            }
            playVideo
          />
          <Show when={isOwner()}>
            <button
              type="button"
              class="lk-icon-button lk-profile-avatar-edit"
              title={t("profile.avatar.change")}
              aria-label={t("profile.avatar.change")}
              aria-haspopup="menu"
              aria-expanded={menuOpen()}
              disabled={uploading() !== null}
              onClick={() => setMenuOpen((v) => !v)}
            >
              <Icon name="pencil" size={20} />
            </button>
          </Show>
          <Show when={menuOpen()}>
            <AvatarMenu
              canRemove={channelAvatarOf(props.id) !== null}
              onClose={() => setMenuOpen(false)}
              onPick={(source) => {
                setMenuOpen(false);
                setCropSource(source);
              }}
              onCamera={() => {
                setMenuOpen(false);
                setCameraOpen(true);
              }}
              onRemove={() => {
                setMenuOpen(false);
                void removePhoto();
              }}
              onUnusable={() => {
                setMenuOpen(false);
                setPhotoError(t("profile.video.unusable"));
              }}
            />
          </Show>
        </div>
        <Show when={photoError()}>
          {(m) => (
            <p class="form-error" role="alert">
              {m()}
              <Show when={lastLocal() !== null}>
                {" "}
                <button
                  type="button"
                  class="ghost small"
                  onClick={() => {
                    const l = lastLocal();
                    if (l !== null) void usePhoto(l);
                  }}
                >
                  {t("common.try_again")}
                </button>
              </Show>
            </p>
          )}
        </Show>

        <Show
          when={editing()}
          fallback={
            <div class="lk-info-title-row">
              <h1>{facts().name}</h1>
              <Show when={has("rename-group")}>
                <button
                  type="button"
                  class="lk-icon-button"
                  title={t("channels.meta.title")}
                  aria-label={t("channels.meta.title")}
                  onClick={() => {
                    setDraftName(facts().name);
                    setDraftDescription(facts().description);
                    setNameError(null);
                    setEditing(true);
                  }}
                >
                  <Icon name="pencil" size={20} />
                </button>
              </Show>
            </div>
          }
        >
          <form
            class="lk-stack"
            onSubmit={(e) => {
              e.preventDefault();
              void saveName();
            }}
          >
            <label>
              {t("channels.new.name_label")}
              <input
                type="text"
                value={draftName()}
                maxLength={64}
                disabled={saving()}
                onInput={(e) => setDraftName(e.currentTarget.value)}
              />
            </label>
            <label>
              {t("channels.new.description_label")}
              <textarea
                rows={3}
                value={draftDescription()}
                disabled={saving()}
                onInput={(e) => setDraftDescription(e.currentTarget.value)}
              />
            </label>
            <Show when={nameError()}>{(m) => <p class="form-error" role="alert">{m()}</p>}</Show>
            <div class="lk-actions">
              <button type="submit" class="primary" disabled={saving()}>
                {saving() ? t("common.saving") : t("common.save")}
              </button>
              <button type="button" class="ghost" disabled={saving()} onClick={() => setEditing(false)}>
                {t("common.cancel")}
              </button>
            </div>
          </form>
        </Show>

        <p class="lk-info-header-line">
          {channelHeaderLine(channelVisibilityOf(props.id), publishes() ? (count() ?? null) : null)}
        </p>
        <Show when={has("handle") ? handle() : null}>
          {(h) => (
            <div class="lk-card-row">
              <span class="lk-card-row-icon" aria-hidden="true">
                @
              </span>
              <div class="lk-card-row-body">
                <span class="lk-card-row-title" dir="ltr">{`@${h()}`}</span>
              </div>
              <button
                type="button"
                class="lk-card-row-action"
                aria-label={t("group_info.handle.copy_a11y")}
                onClick={() =>
                  void copyMessageText(`@${h()}`).then((ok) => {
                    setHandleCopy(ok ? "copied" : "failed");
                    if (ok) window.setTimeout(() => setHandleCopy((s) => (s === "copied" ? "idle" : s)), 2000);
                  })
                }
              >
                {handleCopy() === "copied" ? t("common.copied") : t("common.copy")}
              </button>
            </div>
          )}
        </Show>
        {/* UX 4.4 — a channel whose @name lapsed: the owner chooses one. */}
        <Show when={isOwner() && handle() === null && has("group-type")}>
          <div class="lk-banner" role="status">
            <span>{t("channels.info.handle_lapsed")}</span>
            <button type="button" class="ghost small" onClick={() => setScreen("type")}>
              {t("group_info.handle.choose")}
            </button>
          </div>
        </Show>
        <Show when={handleCopy() === "failed"}>
          <p class="form-error" role="alert">
            {t("common.copy_failed")}
          </p>
        </Show>
        <Show when={!editing() && has("channel-description") && facts().description.length > 0}>
          <p class="lk-meta lk-info-description">{facts().description}</p>
        </Show>
      </section>

      <section class="lk-card">
        <Show when={has("invite-people")}>
          <InfoRow icon="person-plus" title={t("channels.info.invite_people")} onOpen={() => setScreen("invite-people")} />
        </Show>
        <Show when={has("invite-link")}>
          <InfoRow icon="link" title={t("chat.invite_link.label")} onOpen={() => setScreen("invite-link")} />
        </Show>
        <Show when={has("subscribers")}>
          <InfoRow
            icon="person-2"
            title={t("channels.subscribers.title")}
            value={publishes() && typeof count() === "number" ? (count() ?? 0).toLocaleString() : undefined}
            onOpen={() => setScreen("subscribers")}
          />
        </Show>
        <Show when={has("admins")}>
          <InfoRow
            icon="person"
            title={t("channels.info.publishers")}
            value={String(channelPublishers(props.id).length)}
            onOpen={() => setScreen("publishers")}
          />
        </Show>
        <Show when={has("reactions")}>
          <InfoRow
            icon="face-smiling"
            title={t("group_reactions.title")}
            value={allowedSummary(channelAllowedReactions(props.id))}
            onOpen={() => setScreen("reactions")}
          />
        </Show>
        <Show when={has("group-type")}>
          <InfoRow
            icon="person-2"
            title={t("channel_type.who_can_subscribe")}
            value={channelVisibilityOf(props.id) === "public" ? t("group_type.public") : t("group_type.private")}
            onOpen={() => setScreen("type")}
          />
        </Show>
      </section>

      <Show when={has("members-can-find-me")}>
        <MembersCanFindMeSwitch kind="channel" idHex={props.id} />
      </Show>

      <Show when={has("unsubscribe")}>
        <section class="lk-card danger">
          <div class="lk-card-row">
            <span class="lk-card-row-icon">
              <Icon name="logout" size={20} />
            </span>
            <div class="lk-card-row-body">
              <span class="lk-card-row-title">{t("channels.unsubscribe.action")}</span>
              <span class="lk-card-row-sub">{t("channels.info.unsubscribe_sub")}</span>
            </div>
            <button type="button" class="danger lk-card-row-action" onClick={() => void unsubscribe()}>
              {t("channels.unsubscribe.action")}
            </button>
          </div>
        </section>
      </Show>

      <Show when={has("delete-group")}>
        <section class="lk-card danger">
          <div class="lk-card-row">
            <span class="lk-card-row-icon">
              <Icon name="trash" size={20} />
            </span>
            <div class="lk-card-row-body">
              <span class="lk-card-row-title">{t("channels.delete.control")}</span>
            </div>
            <button type="button" class="danger lk-card-row-action" onClick={() => void removeChannel()}>
              {t("common.delete")}
            </button>
          </div>
          <Show when={deleteError()}>
            {(m) => (
              <p class="form-error" role="alert">
                {m()}
              </p>
            )}
          </Show>
        </section>
      </Show>



      <Show when={cropSource()}>
        {(source) => (
          <AvatarCrop
            source={source()}
            onCancel={() => setCropSource(null)}
            onUse={(local) => {
              setCropSource(null);
              void usePhoto(local);
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
    </>
  );
}

/** One row of the info's actions, named by its title and value. */
const InfoRow = InfoRowButton;

/** The screen a channel's info pushed, drawn by the conversation view over
 *  the whole pane (UX 12.1). */
export function ChannelInfoScreenHost(props: {
  readonly id: string;
  readonly screen: ChannelInfoScreen | null;
  readonly onClose: () => void;
}): JSX.Element {
  const chats = useChats();
  const screen = (): Screen | null => props.screen;
  const setScreen = (s: null): void => {
    if (s === null) props.onClose();
  };
  const publishes = (): boolean => {
    const r = channelRoleOf(props.id);
    return r === "owner" || r === "publisher";
  };
  // The exact count, to the owner and the publishers (UX 15.1).
  const [count] = createResource(
    () => (publishes() && props.screen === "subscribers" ? props.id : null),
    async (id) => {
      const identity = chats.conversationAccountIdentity();
      if (identity === null) return null;
      try {
        return await channelSubscriberCount(identity, id);
      } catch {
        return null;
      }
    },
  );
  // Reactions screen: its back handler, while it has unsaved changes.
  let leaveReactions: (() => void) | null = null;

  const screenTitle = (s: Screen): string => {
    switch (s) {
      case "type":
        return t("channel_type.title");
      case "subscribers":
        return t("channels.subscribers.title");
      case "publishers":
        return t("channels.info.publishers");
      case "invite-people":
        return t("channels.info.invite_people");
      case "invite-link":
        return t("chat.invite_link.label");
      case "reactions":
        return t("group_reactions.title");
    }
  };

  return (
      <PushedScreen
        open={screen() !== null}
        title={screen() === null ? "" : screenTitle(screen() as Screen)}
        onBack={() => {
          if (screen() === "reactions" && leaveReactions !== null) {
            leaveReactions();
            return;
          }
          setScreen(null);
        }}
      >
        <Switch>
          <Match when={screen() === "type"}>
            <ChannelTypeScreen id={props.id} onDone={() => setScreen(null)} />
          </Match>
          <Match when={screen() === "subscribers"}>
            <ChannelSubscribersScreen id={props.id} count={publishes() ? (count() ?? null) : null} />
          </Match>
          <Match when={screen() === "publishers"}>
            <ChannelPublishersScreen id={props.id} />
          </Match>
          <Match when={screen() === "invite-people"}>
            <ChannelInvitePeopleScreen id={props.id} onDone={() => setScreen(null)} />
          </Match>
          <Match when={screen() === "invite-link"}>
            <ChannelInviteLinksScreen id={props.id} />
          </Match>
          <Match when={screen() === "reactions"}>
            <AllowedReactionsScreen
              initial={channelAllowedReactions(props.id)}
              save={async (allowed) => {
                const identity = chats.conversationAccountIdentity();
                if (identity === null) throw new Error("sign in");
                await saveChannelReactions(identity, props.id, allowed);
              }}
              onDone={() => setScreen(null)}
              bindLeave={(leave) => {
                leaveReactions = leave;
              }}
              audience="subscribers"
            />
          </Match>
        </Switch>
      </PushedScreen>
  );
}
