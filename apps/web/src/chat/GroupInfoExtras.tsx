// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The ADR-0087 blocks of a group's info screen, `docs/ux/group-and-channel-privacy.md`
 * section 4 and `docs/ux/reactions-and-emoji.md` section 9:
 *
 * - the 96 pt header avatar with the photo editor for the owner and admins
 *   (4.5.2): `Photo or video`, `Camera`, `Remove photo`, no Save;
 * - the header line `Private group · 12 members` / `Public group · ...`;
 * - the `@name` row with `Copy` (4), the lapsed banner for the owner (4.4);
 * - the owner's `Who can join` row, opening the pushed **Group type** screen
 *   (4.1);
 * - the owner's and admins' `Reactions` row, opening the pushed **Reactions**
 *   screen (reactions section 9).
 *
 * Everything sent goes through `GroupNameContext` (the role gate on every
 * peer is the boundary; the role read here only decides what is offered).
 */
import { createMemo, createSignal, For, onCleanup, Show, type JSX } from "solid-js";

import { useChats, type ActiveGroup } from "./ChatContext";
import { useGroupNames } from "./GroupNameContext";
import { t } from "../l10n";
import { Avatar } from "../ui/Avatar";
import { Icon } from "../ui/icons";
import { PushedScreen } from "../ui/PushedScreen";
import { LakkoPicker } from "../ui/LakkoPicker";
import { requestConfirm } from "../ui/confirm-dialog";
import { AvatarCamera, AvatarCrop, AvatarMenu, UnsavedChangesDialog, type CropSource } from "../profile/AvatarEditor";
import { releaseLocalAvatar, type LocalAvatar } from "../profile/avatar-media";
import { uploadLocalAvatar } from "../profile/avatar-upload";
import { copyMessageText } from "./copy-message";
import { EmojiPicker } from "../media/EmojiPicker";
import { loadWasm } from "../wasm";
import { knockKeyFor } from "../contacts/client";
import {
  claimGroupHandle,
  handleWellFormed,
  replaceGroupHandle,
  suggestedHandle,
  handleRandom,
  updateGroupHandle,
  type PublicGroupFacts,
} from "./group-handle";
import { HandleWriteError } from "../discovery/handle-lookup";
import {
  dismissHandleFailure,
  handleFailureFor,
  owedHandleFor,
  owedOutcomeOf,
  recordOwedHandle,
} from "./handle-owed";
import { DiscoveryNetworkError } from "../discovery/client";
import { quickReactions } from "../../wasm/lakko_ffi_wasm";
import type { AllowedSetting } from "./reaction-rules";
import type { InfoSection } from "./conversation-info";
import { avatarCacheBytes } from "../profile/avatar-runtime";
import { encryptAndUploadBytes } from "./attachments";
import { bytesToB64, type AvatarData } from "../profile/avatar-model";

export type InfoRole = "owner" | "admin" | "member";

/** The copy of a refused handle write (UX 3.3 / 4.1). */
export function handleWriteErrorText(err: unknown): string {
  if (err instanceof HandleWriteError) {
    if (err.code === "name-taken") return t("group_handle.error.held");
    if (err.status === 429) return t("group_handle.error.too_many");
    return t("group_handle.error.failed");
  }
  if (err instanceof DiscoveryNetworkError) return t("group_handle.error.offline");
  return t("group_handle.error.failed");
}

/** The header line of a group's info (section 4). */
export function groupHeaderLine(isPublic: boolean, members: number): string {
  return isPublic
    ? t("group_info.header.public", { count: members })
    : t("group_info.header.private", { count: members });
}

export function GroupInfoExtras(props: {
  readonly entry: ActiveGroup;
  readonly role: InfoRole;
  readonly memberCount: number;
  readonly title: string;
  readonly groupName: string | null;
  readonly ended: boolean;
  /** What the info holds, from the shared rule (`conversationInfoSections`). */
  readonly sections: readonly InfoSection[];
}): JSX.Element {
  const chats = useChats();
  const groupNames = useGroupNames();
  /** The Reactions screen's back handler (it asks before losing changes). */
  let reactionsLeave: (() => void) | null = null;
  /** A change of who can join still owed (`handle-owed.ts`). */
  const owedChange = () => {
    const o = owedHandleFor("group", props.entry.groupIdB64);
    return o !== undefined && o.reason === "change" ? o : null;
  };
  const canSetPhoto = (): boolean => !props.ended && (props.role === "owner" || props.role === "admin");
  const handle = createMemo(() => groupNames.handleOf(props.entry.groupIdB64));
  const photo = createMemo(() => groupNames.photoOf(props.entry.groupIdB64));

  // ----- photo (4.5.2) -----
  const [menuOpen, setMenuOpen] = createSignal(false);
  const [cropSource, setCropSource] = createSignal<CropSource | null>(null);
  const [cameraOpen, setCameraOpen] = createSignal(false);
  const [uploading, setUploading] = createSignal<LocalAvatar | null>(null);
  const [photoError, setPhotoError] = createSignal<string | null>(null);
  const [lastLocal, setLastLocal] = createSignal<LocalAvatar | null>(null);

  async function usePhoto(local: LocalAvatar): Promise<void> {
    setPhotoError(null);
    setUploading(local);
    setLastLocal(local);
    try {
      const avatar = await uploadLocalAvatar(local);
      if (!canSetPhoto()) {
        setPhotoError(t("group_photo.error.not_allowed"));
        return;
      }
      await groupNames.setPhoto(props.entry, avatar);
      releaseLocalAvatar(local);
      setLastLocal(null);
    } catch (err) {
      const text = err instanceof Error ? err.message : "";
      setPhotoError(
        /limit/i.test(text) || /429/.test(text) ? t("group_photo.error.limit") : t("group_photo.error.upload"),
      );
    } finally {
      setUploading(null);
    }
  }

  async function removePhoto(): Promise<void> {
    const ok = await requestConfirm({
      title: t("group_photo.remove.title"),
      body: t("group_photo.remove.body"),
      confirmLabel: t("group_photo.remove.confirm"),
      cancelLabel: t("common.cancel"),
      tone: "danger",
    });
    if (!ok) return;
    setPhotoError(null);
    try {
      await groupNames.setPhoto(props.entry, null);
    } catch {
      setPhotoError(t("group_photo.error.upload"));
    }
  }

  // ----- screens -----
  const [screen, setScreen] = createSignal<"info" | "type" | "reactions">("info");

  return (
    <>
      <section class="lk-card lk-info-header" aria-label={t("group_info.header.aria")}>
        <div class="lk-profile-avatar lk-menu-anchor" classList={{ uploading: uploading() !== null }}>
          <Avatar
            label={props.title}
            id={props.entry.groupIdB64}
            size="profile"
            conversationKey={props.entry.groupIdB64}
            media={
              uploading() !== null
                ? { stillUrl: uploading()?.stillUrl ?? null, videoUrl: uploading()?.videoUrl ?? null }
                : undefined
            }
            playVideo
          />
          <Show when={canSetPhoto()}>
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
              <Icon name="pencil" size={16} />
            </button>
          </Show>
          <Show when={menuOpen()}>
            <AvatarMenu
              canRemove={photo()?.avatar !== null && photo()?.avatar !== undefined}
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
        <p class="lk-info-header-line">{groupHeaderLine(handle()?.isPublic === true, props.memberCount)}</p>
        <Show when={props.sections.includes("handle") ? handle() : undefined}>
          {(h) => (
            <div class="lk-card-row">
              <span class="lk-card-row-icon" aria-hidden="true">
                @
              </span>
              <div class="lk-card-row-body">
                <span class="lk-card-row-title" dir="ltr">{`@${h().handle}`}</span>
              </div>
              <button
                type="button"
                class="lk-card-row-action"
                aria-label={t("group_info.handle.copy_a11y")}
                onClick={() => void copyMessageText(`@${h().handle}`)}
              >
                {t("common.copy")}
              </button>
            </div>
          )}
        </Show>
        <Show when={!props.sections.includes("handle") && props.sections.includes("group-type")}>
          <div class="lk-banner" role="status">
            <span>{t("group_info.handle.lapsed")}</span>
            <button type="button" class="ghost small" onClick={() => setScreen("type")}>
              {t("group_info.handle.choose")}
            </button>
          </div>
        </Show>
        <Show when={props.sections.includes("group-type")}>
          <button type="button" class="lk-card-row lk-card-row-button" onClick={() => setScreen("type")}>
            <span class="lk-card-row-icon">
              <Icon name="eye" size={18} />
            </span>
            <span class="lk-card-row-body">
              <span class="lk-card-row-title">{t("group_type.who_can_join")}</span>
            </span>
            <span class="lk-card-row-sub">
              <Show
                when={owedChange()}
                fallback={handle()?.isPublic === true ? t("group_type.public") : t("group_type.private")}
              >
                {(o) => (
                  <>
                    {o().isPublic ? t("group_type.public") : t("group_type.private")}
                    {" · "}
                    {t("group_type.pending")}
                  </>
                )}
              </Show>
            </span>
          </button>
          <Show when={handleFailureFor(props.entry.groupIdB64)}>
            <div class="lk-banner" role="alert">
              <span>{t("group_type.not_saved")}</span>
              <button
                type="button"
                class="ghost small"
                onClick={() => dismissHandleFailure(props.entry.groupIdB64)}
              >
                {t("common.dismiss")}
              </button>
            </div>
          </Show>
        </Show>
        {/* reactions-and-emoji.md section 9 — from the shared info rule. */}
        <Show when={props.sections.includes("reactions")}>
          <button type="button" class="lk-card-row lk-card-row-button" onClick={() => setScreen("reactions")}>
            <span class="lk-card-row-icon">
              <Icon name="heart" size={18} />
            </span>
            <span class="lk-card-row-body">
              <span class="lk-card-row-title">{t("group_reactions.title")}</span>
            </span>
            <span class="lk-card-row-sub">{allowedSummary(groupNames.reactionsOf(props.entry.groupIdB64))}</span>
          </button>
        </Show>
      </section>

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

      <PushedScreen open={screen() === "type"} title={t("group_type.title")} onBack={() => setScreen("info")}>
        <GroupTypeScreen
          entry={props.entry}
          groupName={props.groupName}
          memberCount={props.memberCount}
          onDone={() => setScreen("info")}
          accountIdentity={() => chats.accountIdentity()}
        />
      </PushedScreen>
      <PushedScreen
        open={screen() === "reactions"}
        title={t("group_reactions.title")}
        onBack={() => (reactionsLeave !== null ? reactionsLeave() : setScreen("info"))}
      >
        <ReactionsScreen
          entry={props.entry}
          onDone={() => setScreen("info")}
          bindLeave={(leave) => {
            reactionsLeave = leave;
          }}
        />
      </PushedScreen>
    </>
  );
}

/** The `Reactions` row's value: `All`, `Off`, or the number chosen. */
export function allowedSummary(a: AllowedSetting): string {
  if (a.mode === "all") return t("group_reactions.value.all");
  if (a.mode === "off") return t("group_reactions.value.off");
  return t("group_reactions.value.count", { count: a.emoji.length });
}

/**
 * The photo in effect, re-uploaded under a fresh key (ADR-0087 section 18.3:
 * going private sends the same photo again in a form only members can open).
 * `null` when this device does not hold the bytes.
 */
export async function reuploadedPhoto(a: AvatarData | null): Promise<AvatarData | null> {
  if (a === null) return null;
  const still = avatarCacheBytes(a.still.blobIdB64);
  if (still === null) return null;
  const s = await encryptAndUploadBytes(still, { mime: "image/jpeg", name: undefined });
  let video: AvatarData["video"] = null;
  if (a.video !== null) {
    const vb = avatarCacheBytes(a.video.ref.blobIdB64);
    if (vb !== null) {
      const v = await encryptAndUploadBytes(vb, { mime: "video/mp4", name: undefined });
      video = {
        ref: { blobIdB64: bytesToB64(v.blobId), keyB64: bytesToB64(v.key), nonceB64: bytesToB64(v.nonce), size: vb.length, side: a.video.ref.side },
        durationMs: a.video.durationMs,
      };
    }
  }
  return {
    still: { blobIdB64: bytesToB64(s.blobId), keyB64: bytesToB64(s.key), nonceB64: bytesToB64(s.nonce), size: still.length, side: a.still.side },
    video,
  };
}

/** Section 4.1 — the pushed **Group type** screen (the owner's). */
function GroupTypeScreen(props: {
  readonly entry: ActiveGroup;
  readonly groupName: string | null;
  readonly memberCount: number;
  readonly onDone: () => void;
  readonly accountIdentity: () => ReturnType<ReturnType<typeof useChats>["accountIdentity"]>;
}): JSX.Element {
  const chats = useChats();
  const groupNames = useGroupNames();
  const held = (): ReturnType<typeof groupNames.handleOf> => groupNames.handleOf(props.entry.groupIdB64);
  // A change still owed is what the screen opens on.
  const owed = owedHandleFor("group", props.entry.groupIdB64);
  const [isPublic, setIsPublic] = createSignal<boolean>(owed?.isPublic ?? held()?.isPublic === true);
  const [handleText, setHandleText] = createSignal<string>(owed?.handle ?? held()?.handle ?? "");
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  const [wellFormed, setWellFormed] = createSignal(true);

  void loadWasm().then((wasm) => {
    if (handleText().length === 0) {
      setHandleText(suggestedHandle(wasm, props.groupName ?? "", !isPublic(), handleRandom()));
    }
  });

  const needsName = (): boolean => isPublic() && (props.groupName ?? "").trim().length === 0;
  const changed = (): boolean => {
    const h = held();
    return h === undefined || h.isPublic !== isPublic() || h.handle !== handleText().replace(/^@/, "");
  };

  async function save(): Promise<void> {
    const wasm = await loadWasm();
    const typed = handleText().replace(/^@/, "").trim();
    if (!handleWellFormed(wasm, typed)) {
      setWellFormed(false);
      return;
    }
    const h = held();
    const flipping = h !== undefined && h.isPublic !== isPublic();
    const renaming = h !== undefined && h.handle !== typed;
    if (flipping) {
      const ok = await requestConfirm(
        isPublic()
          ? {
              title: t("group_type.confirm.public.title"),
              body: t("group_type.confirm.public.body"),
              confirmLabel: t("group_type.confirm.public.confirm"),
              cancelLabel: t("common.cancel"),
              tone: "neutral",
            }
          : {
              title: t("group_type.confirm.private.title"),
              body: t("group_type.confirm.private.body"),
              confirmLabel: t("group_type.confirm.private.confirm"),
              cancelLabel: t("common.cancel"),
              tone: "neutral",
            },
      );
      if (!ok) return;
    }
    if (renaming) {
      const ok = await requestConfirm({
        title: t("group_type.confirm.rename.title"),
        body: t("group_type.confirm.rename.body"),
        confirmLabel: t("group_type.confirm.rename.confirm"),
        cancelLabel: t("common.cancel"),
        tone: "neutral",
      });
      if (!ok) return;
    }
    const account = props.accountIdentity();
    if (account === null) {
      setError(t("group_handle.error.failed"));
      return;
    }
    setBusy(true);
    setError(null);
    try {
      let facts: PublicGroupFacts | null = null;
      if (isPublic()) {
        const knock = await knockKeyFor(wasm, account, props.entry.group, chats.conversationAccountIdentity());
        if (knock === null) throw new Error("no knock key");
        facts = {
          name: (props.groupName ?? "").trim(),
          memberCount: props.memberCount,
          knockPublicKey: knock.publicKey,
      knockVersion: knock.version,
          avatar: groupNames.photoOf(props.entry.groupIdB64)?.avatar ?? null,
        };
      }
      const common = { wasm, accountIdentity: account, groupId: props.entry.group.groupId, isPublic: isPublic(), facts };
      const next =
        h === undefined
          ? await claimGroupHandle({ ...common, handle: typed })
          : renaming
            ? await replaceGroupHandle({ ...common, oldHandle: h.handle, newHandle: typed })
            : await updateGroupHandle({ ...common, handle: typed });
      await groupNames.setHandle(props.entry, next);
      // Section 18.3 — going private sends the photo again under a fresh key.
      if (flipping && !isPublic()) {
        const current = groupNames.photoOf(props.entry.groupIdB64)?.avatar ?? null;
        try {
          const fresh = await reuploadedPhoto(current);
          if (fresh !== null) await groupNames.setPhoto(props.entry, fresh);
        } catch (err) {
          // eslint-disable-next-line no-console
          console.warn("[group-type] photo re-upload failed", err instanceof Error ? err.name : "");
        }
      }
      props.onDone();
    } catch (err) {
      // A write that did not land for a reason that can pass (no network, a
      // server error, a token pool that ran dry) is owed and retried until it
      // lands; a refusal for good is said here.
      if (owedOutcomeOf(err) === "retry") {
        try {
          await recordOwedHandle({
            kind: "group",
            id: props.entry.groupIdB64,
            op: h === undefined ? "claim" : renaming ? "replace" : "update",
            handle: typed,
            oldHandle: renaming && h !== undefined ? h.handle : null,
            isPublic: isPublic(),
            sinceMs: Date.now(),
            reason: "change",
          });
          props.onDone();
          return;
        } catch {
          /* not recorded: said below */
        }
      }
      setError(handleWriteErrorText(err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div class="lk-stack" style={{ padding: "16px" }}>
      <section class="lk-card">
        <label id="lk-group-type-who">{t("group_type.who_can_join")}</label>
        <LakkoPicker
          value={isPublic() ? "public" : "private"}
          labelledBy="lk-group-type-who"
          options={[
            { value: "private", label: t("group_type.private") },
            { value: "public", label: t("group_type.public") },
          ]}
          disabled={busy()}
          onChange={(v) => setIsPublic(v === "public")}
        />
        <p class="lk-card-desc">{isPublic() ? t("group_type.public_desc") : t("group_type.private_desc")}</p>
        <label>
          {t("group_type.handle")}
          <input
            type="text"
            dir="ltr"
            value={`@${handleText().replace(/^@/, "")}`}
            maxLength={33}
            autocomplete="off"
            disabled={busy()}
            aria-invalid={!wellFormed()}
            onInput={(e) => {
              setHandleText(e.currentTarget.value.replace(/^@/, ""));
              setWellFormed(true);
            }}
          />
        </label>
        <p class="lk-card-desc">{isPublic() ? t("group_type.handle_public") : t("group_type.handle_private")}</p>
        <Show when={!wellFormed()}>
          <p class="form-error" role="alert">
            {t("group_handle.error.bad_form")}
          </p>
        </Show>
        <Show when={needsName()}>
          <p class="form-error" role="alert">
            {t("group_type.needs_name")}
          </p>
        </Show>
        <Show when={error()}>
          {(m) => (
            <p class="form-error" role="alert">
              {m()}
            </p>
          )}
        </Show>
      </section>
      <div class="lk-actions">
        <button
          type="button"
          class="primary"
          disabled={busy() || !changed() || needsName()}
          onClick={() => void save()}
        >
          {busy() ? t("common.saving") : t("common.save")}
        </button>
      </div>
    </div>
  );
}

/** Reactions section 9 — the pushed **Reactions** screen (owner, admins). */
function ReactionsScreen(props: {
  readonly entry: ActiveGroup;
  readonly onDone: () => void;
  /** Hands the screen's back handler to the pushed screen around it. */
  readonly bindLeave: (leave: (() => void) | null) => void;
}): JSX.Element {
  const groupNames = useGroupNames();
  const [asking, setAsking] = createSignal(false);
  const initial = (): AllowedSetting => groupNames.reactionsOf(props.entry.groupIdB64);
  const [mode, setMode] = createSignal<AllowedSetting["mode"]>(initial().mode);
  const [chosen, setChosen] = createSignal<string[]>(initial().mode === "only" ? [...(initial() as { emoji: readonly string[] }).emoji] : []);
  const [picking, setPicking] = createSignal(false);
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  const MAX = 100;

  const current = (): AllowedSetting =>
    mode() === "only" ? { mode: "only", emoji: chosen() } : mode() === "off" ? { mode: "off" } : { mode: "all" };
  const changed = (): boolean => JSON.stringify(current()) !== JSON.stringify(initial());
  const valid = (): boolean => mode() !== "only" || chosen().length > 0;

  function pick(m: AllowedSetting["mode"]): void {
    setMode(m);
    // The first time `Some` is picked the grid starts with the six defaults.
    if (m === "only" && chosen().length === 0) {
      try {
        setChosen(quickReactions());
      } catch {
        setChosen([]);
      }
    }
  }

  async function save(): Promise<void> {
    if (!valid()) return;
    setBusy(true);
    setError(null);
    try {
      await groupNames.setReactions(props.entry, current());
      setAsking(false);
      props.onDone();
    } catch {
      setAsking(false);
      setError(t("group_reactions.save_failed"));
    } finally {
      setBusy(false);
    }
  }

  /** Back with nothing saved: what was chosen goes back to what is set. */
  function reset(): void {
    const a = initial();
    setMode(a.mode);
    setChosen(a.mode === "only" ? [...a.emoji] : []);
    setError(null);
  }

  // Section 9 — leaving with unsaved changes asks "Save changes?" (the
  // profile screen's dialog).
  props.bindLeave(() => {
    if (changed()) {
      setAsking(true);
      return;
    }
    props.onDone();
  });
  onCleanup(() => props.bindLeave(null));

  const options: ReadonlyArray<{ value: AllowedSetting["mode"]; title: string; sub: string }> = [
    { value: "all", title: "group_reactions.all", sub: "group_reactions.all_sub" },
    { value: "only", title: "group_reactions.some", sub: "group_reactions.some_sub" },
    { value: "off", title: "group_reactions.none", sub: "group_reactions.none_sub" },
  ];

  return (
    <div class="lk-stack" style={{ padding: "16px" }}>
      <section class="lk-card" role="radiogroup" aria-label={t("group_reactions.title")}>
        <For each={options}>
          {(o) => (
            <label class="lk-card-row lk-radio-row">
              <input type="radio" name="lk-reactions-mode" checked={mode() === o.value} onChange={() => pick(o.value)} />
              <span class="lk-card-row-body">
                <span class="lk-card-row-title">{t(o.title)}</span>
                <span class="lk-card-row-sub">{t(o.sub)}</span>
              </span>
            </label>
          )}
        </For>
      </section>
      <Show when={mode() === "only"}>
        <section class="lk-card" style={{ position: "relative" }}>
          <div class="lk-reactions-grid">
            <For each={chosen()}>
              {(emoji, i) => (
                <button
                  type="button"
                  draggable={true}
                  aria-label={t("group_reactions.remove_a11y", { emoji })}
                  onDragStart={(e) => e.dataTransfer?.setData("text/plain", String(i()))}
                  onDragOver={(e) => e.preventDefault()}
                  onDrop={(e) => {
                    e.preventDefault();
                    const from = Number(e.dataTransfer?.getData("text/plain"));
                    if (!Number.isInteger(from)) return;
                    const list = [...chosen()];
                    const [moved] = list.splice(from, 1);
                    if (moved === undefined) return;
                    list.splice(i(), 0, moved);
                    setChosen(list);
                  }}
                  onClick={() => setChosen(chosen().filter((_, j) => j !== i()))}
                >
                  {emoji}
                </button>
              )}
            </For>
          </div>
          <Show when={chosen().length === 0}>
            <p class="form-error">{t("group_reactions.choose_one")}</p>
          </Show>
          <button type="button" data-picker-toggle onClick={() => setPicking(!picking())}>
            {t("group_reactions.add")}
          </button>
          <Show when={picking()}>
            <EmojiPicker
              mode="reaction"
              stayOpen
              isDisabled={(emoji) => chosen().includes(emoji) || chosen().length >= MAX}
              notice={chosen().length >= MAX ? t("group_reactions.max") : undefined}
              onEmoji={(emoji) => {
                if (!chosen().includes(emoji) && chosen().length < MAX) setChosen([...chosen(), emoji]);
              }}
              onClose={() => setPicking(false)}
              sessionToken={() => null}
            />
          </Show>
        </section>
      </Show>
      <Show when={error()}>
        {(m) => (
          <p class="form-error" role="alert">
            {m()}
          </p>
        )}
      </Show>
      <div class="lk-actions">
        <button type="button" class="primary" disabled={busy() || !changed() || !valid()} onClick={() => void save()}>
          {busy() ? t("common.saving") : t("common.save")}
        </button>
      </div>
      <UnsavedChangesDialog
        open={asking()}
        busy={busy()}
        body={t("group_reactions.unsaved_body")}
        onSave={() => {
          if (!valid()) {
            setAsking(false);
            return;
          }
          void save();
        }}
        onDiscard={() => {
          setAsking(false);
          reset();
          props.onDone();
        }}
        onCancel={() => setAsking(false)}
      />
    </div>
  );
}
