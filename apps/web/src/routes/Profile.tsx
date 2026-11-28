// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Settings > Profile (docs/ux/profile-screen.md, ADR-0084): the avatar, the
 * name and the username, each edited in place, one Save.
 *
 * Every rule lives in `profile/profile-edit.ts` (the saved / pending model,
 * the in-place field, the save order, the leave guard), which is tested;
 * this file is the markup and the wiring. The username change is the
 * existing ADR-0076 operation (`runRename` over `useRenamePorts`), driven
 * from Save, with its proof block opening over the screen when needed.
 */
import { createEffect, createMemo, createSignal, onCleanup, Show, untrack, type JSX } from "solid-js";
import { useBeforeLeave, type BeforeLeaveEventArgs } from "@solidjs/router";

import { useAuth } from "../auth/AuthContext";
import { PROOF_COPY } from "../auth/fresh-auth";
import { ProofBlock } from "../auth/ProofBlock";
import { createProofGate } from "../auth/proof-gate";
import { rememberAddress } from "../auth/protect/address-register";
import { plainStore } from "../auth/protect/local";
import { refreshProtection } from "../auth/protect/protection-state";
import {
  failureText,
  renameAsksForAddress,
  renameEmailFor,
  useRenamePorts,
} from "../auth/rename-runtime";
import { readRenameOwed, rollbackRename, runRename, type RenameOwed } from "../auth/username-change";
import { encryptAndUploadBytes } from "../chat/attachments";
import { t } from "../l10n";
import { AvatarCamera, AvatarCrop, AvatarMenu, UnsavedChangesDialog, type CropSource } from "../profile/AvatarEditor";
import { releaseLocalAvatar, type LocalAvatar } from "../profile/avatar-media";
import { b64ToBytes, bytesToB64, type AvatarData, type AvatarRefData } from "../profile/avatar-model";
import { bytesToHex as blobHex } from "../chat/blob-ids";
import { deleteBlobs } from "../storage/blob-deletes";
import { OWN_AVATAR_KEY, seedAvatarBytes, type AvatarMedia } from "../profile/avatar-runtime";
import {
  CLOSED_FIELD,
  isDirty,
  leaveAnswerEffect,
  leaveDecision,
  leaveField,
  normalizePending,
  openField,
  photoShown,
  revertField,
  runProfileSave,
  saveEnabled,
  typeIntoField,
  type FieldState,
  type LeaveAnswer,
  type PendingFields,
  type RenameResult,
  type SavedFields,
  type SaveError,
} from "../profile/profile-edit";
import { useProfiles } from "../profile/ProfileContext";
import { maySaveDisplayName, mayLookUp, savedDisplayName, USERNAME_REASON, usernameReason } from "../profile/rules";
import { createDisplayNameCheck, createUsernameCheck } from "../profile/username-field";
import { Avatar } from "../ui/Avatar";
import { Icon } from "../ui/icons";
import { ModalLayer } from "../ui/ModalLayer";

/** How long the success line stays (section 5: "a few seconds"). */
const DONE_NOTICE_MS = 4000;

type Pending = PendingFields<LocalAvatar>;

function mediaOfPending(p: Pending): AvatarMedia | null | undefined {
  const photo = p.photo;
  if (photo === undefined) return undefined;
  if (photo.kind === "remove") return null;
  return { stillUrl: photo.media.stillUrl, videoUrl: photo.media.videoUrl };
}

async function uploadRef(blob: Blob, side: number, mime: string): Promise<{ ref: AvatarRefData; bytes: Uint8Array }> {
  const bytes = new Uint8Array(await blob.arrayBuffer());
  const r = await encryptAndUploadBytes(bytes, { mime, name: undefined });
  return {
    ref: {
      blobIdB64: bytesToB64(r.blobId),
      keyB64: bytesToB64(r.key),
      nonceB64: bytesToB64(r.nonce),
      size: bytes.length,
      side,
    },
    bytes,
  };
}

export default function Profile(): JSX.Element {
  const auth = useAuth();
  const profiles = useProfiles();
  const portsFor = useRenamePorts();

  const account = (): { accountIdB64: string; accountId: Uint8Array; token: string } | null => {
    const s = auth.state();
    const secrets = auth.identitySecrets();
    if (s.kind !== "session" || secrets === null) return null;
    return { accountIdB64: s.account.accountId, accountId: secrets.accountId, token: secrets.sessionToken };
  };

  const saved = createMemo<SavedFields>(() => ({
    name: profiles.ownProfile().displayName,
    username: profiles.ownUsername(),
    avatar: profiles.ownProfile().avatar,
  }));
  const [pending, setPending] = createSignal<Pending>({});
  const [nameField, setNameField] = createSignal<FieldState>(CLOSED_FIELD);
  const [userField, setUserField] = createSignal<FieldState>(CLOSED_FIELD);
  const [busy, setBusy] = createSignal(false);
  const [notice, setNotice] = createSignal<string | null>(null);
  const [error, setError] = createSignal<string | null>(null);
  const [menuOpen, setMenuOpen] = createSignal(false);
  const [cropSource, setCropSource] = createSignal<CropSource | null>(null);
  const [cameraOpen, setCameraOpen] = createSignal(false);
  const [leaving, setLeaving] = createSignal<BeforeLeaveEventArgs | null>(null);
  // UX 19.1c: the recovery email is the account's last second way in and
  // this device does not remember the address, so the username change asks
  // for it once and carries it (the email then moves to the new name).
  const [renameAddress, setRenameAddress] = createSignal("");
  const asksRenameAddress = (): boolean =>
    effectivePending().username !== undefined && renameAsksForAddress(account()?.accountIdB64 ?? null);
  let noticeTimer: ReturnType<typeof setTimeout> | null = null;

  const nameText = (): string => {
    const f = nameField();
    return f.open ? f.text : "";
  };
  const userText = (): string => {
    const f = userField();
    return f.open ? f.text : "";
  };
  const nameCheck = createDisplayNameCheck(nameText);
  const userCheck = createUsernameCheck(userText);

  const shownName = (): string | null => {
    const p = pending();
    return p.name !== undefined ? p.name : saved().name;
  };
  const shownUsername = (): string | null => pending().username ?? saved().username;

  /** The pending state with any open, valid field folded in: what Save would
   *  save right now. */
  const effectivePending = (): Pending => {
    let p = pending();
    if (nameField().open && maySaveDisplayName(nameCheck())) p = { ...p, name: savedDisplayName(nameCheck()) };
    const u = userCheck();
    if (userField().open && mayLookUp(u)) p = { ...p, username: u.normalised };
    return normalizePending(saved(), p);
  };
  const openFieldsValid = (): boolean =>
    (!nameField().open || maySaveDisplayName(nameCheck())) && (!userField().open || mayLookUp(userCheck()));
  const dirty = (): boolean => isDirty(saved(), effectivePending());
  const canSave = (): boolean =>
    saveEnabled({ saved: saved(), pending: effectivePending(), openFieldsValid: openFieldsValid(), busy: busy() });

  // Clear the success line as soon as something is edited again.
  createEffect(() => {
    if (dirty()) setNotice(null);
  });

  // -------------------------------------------------------------------------
  // The fields
  // -------------------------------------------------------------------------

  let nameInput: HTMLInputElement | undefined;
  let userInput: HTMLInputElement | undefined;

  function openName(): void {
    if (busy() || nameField().open) return;
    setNameField(openField(shownName() ?? ""));
    queueMicrotask(() => {
      nameInput?.focus();
      nameInput?.select();
    });
  }
  function leaveName(): void {
    const f = nameField();
    if (!f.open) return;
    const c = nameCheck();
    const left = leaveField(f, maySaveDisplayName(c));
    setNameField(left.state);
    if (left.committed !== null) setPending((p) => ({ ...p, name: savedDisplayName(c) }));
  }
  function revertName(): void {
    const r = revertField(nameField());
    setNameField(r.state);
    if (r.restored !== null) {
      const before = r.restored.trim().length === 0 ? null : r.restored;
      setPending((p) => normalizePending(saved(), { ...p, name: before }));
    }
  }

  function openUser(): void {
    if (busy() || userField().open) return;
    const acct = account();
    if (acct === null) return;
    // An earlier change whose replace was refused left the account's handle
    // the new name's while its name is the old one: binding the old handle
    // back (with its proof) comes first, then the field opens.
    const owed = readRenameOwed(plainStore(), acct.accountIdB64);
    if (owed?.phase === "rollback") {
      void finishOwedRollback(owed);
      return;
    }
    setUserField(openField(shownUsername() ?? ""));
    queueMicrotask(() => {
      userInput?.focus();
      userInput?.select();
    });
  }
  async function finishOwedRollback(record: RenameOwed): Promise<void> {
    const acct = account();
    if (acct === null) return;
    setBusy(true);
    setError(null);
    let done = false;
    try {
      await refreshProtection(acct.accountIdB64, acct.token).catch(() => undefined);
      const grant = await obtainGrant();
      if (grant === null) return;
      const ports = portsFor(acct);
      const ok = await rollbackRename(ports, {
        oldUsername: record.oldUsername,
        newUsername: record.newUsername,
        grant,
        email: renameEmailFor(acct.accountIdB64, renameAddress()),
      });
      if (ok) {
        ports.saveOwed(null);
        done = true;
      } else {
        setError(t("recovery_phrase.fresh.error.generic"));
      }
    } finally {
      setBusy(false);
    }
    if (done) openUser();
  }

  function leaveUser(): void {
    const f = userField();
    if (!f.open) return;
    const c = userCheck();
    const left = leaveField(f, mayLookUp(c));
    setUserField(left.state);
    if (left.committed !== null && mayLookUp(c)) {
      const name = c.normalised;
      setPending((p) => normalizePending(saved(), { ...p, username: name }));
    }
  }
  function revertUser(): void {
    const r = revertField(userField());
    setUserField(r.state);
    if (r.restored !== null) {
      const before = r.restored;
      setPending((p) => {
        const { username: _u, ...rest } = p;
        void _u;
        return normalizePending(saved(), before.length === 0 ? rest : { ...rest, username: before });
      });
    }
  }
  const userReason = (): string | null => {
    const c = userCheck();
    if (c.kind === "blank") return USERNAME_REASON.empty;
    return usernameReason(c);
  };

  // -------------------------------------------------------------------------
  // The photo
  // -------------------------------------------------------------------------

  /** ADR-0065 section 6 — a photo uploaded in a save that stopped at a later
   *  step and then dropped was never published: its blobs go. */
  function deleteUnpublished(photo: Pending["photo"]): void {
    const acct = account();
    if (photo?.kind !== "uploaded" || acct === null) return;
    const ids = [photo.avatar.still.blobIdB64, photo.avatar.video?.ref.blobIdB64]
      .filter((b): b is string => b !== undefined)
      .map((b) => blobHex(b64ToBytes(b)));
    void deleteBlobs(acct.accountIdB64, ids);
  }

  function setPhoto(next: Pending["photo"]): void {
    setPending((p) => {
      const old = p.photo;
      if (old !== undefined && old.kind !== "remove") releaseLocalAvatar(old.media);
      if (old !== next) deleteUnpublished(old);
      const { photo: _old, ...rest } = p;
      void _old;
      return normalizePending(saved(), next === undefined ? rest : { ...rest, photo: next });
    });
  }
  onCleanup(() => {
    const p = untrack(pending).photo;
    if (p !== undefined && p.kind !== "remove") releaseLocalAvatar(p.media);
    deleteUnpublished(p);
  });

  // -------------------------------------------------------------------------
  // The proof block, over the screen (section 5 step 2)
  // -------------------------------------------------------------------------

  const [proofOpen, setProofOpen] = createSignal(false);
  const gate = createProofGate({
    auth,
    copy: PROOF_COPY.changeUsername,
    // The save is busy while it waits for this proof; the gate reads busy as
    // "checking" and disables its field, so the password could never be
    // typed and a rename past the fresh window never finished (base
    // scenario 3, 2026-11-11). While the block is open, only its own check
    // counts.
    busy: () => busy() && !proofOpen(),
    sessionToken: () => account()?.token ?? null,
    usernameKnown: () => (saved().username ?? "").length > 0,
  });
  onCleanup(() => gate.clear());
  let proofAnswer: ((grant: string | null) => void) | null = null;
  const [proofChecking, setProofChecking] = createSignal(false);

  function answerProof(grant: string | null): void {
    setProofOpen(false);
    const answer = proofAnswer;
    proofAnswer = null;
    answer?.(grant);
  }

  /** A live fresh-auth grant, asking for the proof when none is live;
   *  `null` when the person cancelled. */
  async function obtainGrant(): Promise<string | null> {
    if (auth.freshProofLive()) {
      const live = await gate.ensure();
      if (live !== null) return live;
    }
    return new Promise<string | null>((resolve) => {
      proofAnswer = resolve;
      setProofOpen(true);
    });
  }

  async function continueProof(): Promise<void> {
    if (proofChecking()) return;
    setProofChecking(true);
    try {
      const grant = await gate.ensure();
      if (grant !== null) answerProof(grant);
    } finally {
      setProofChecking(false);
    }
  }

  async function rename(newUsername: string): Promise<RenameResult> {
    const acct = account();
    const old = saved().username;
    if (acct === null || old === null) return { kind: "error", text: null };
    await refreshProtection(acct.accountIdB64, acct.token).catch(() => undefined);
    for (let attempt = 0; attempt < 2; attempt += 1) {
      const grant = await obtainGrant();
      if (grant === null) return { kind: "cancelled" };
      const outcome = await runRename(portsFor(acct), {
        oldUsername: old,
        newUsername,
        grant,
        email: renameEmailFor(acct.accountIdB64, renameAddress()),
      });
      if (outcome.kind === "done") return { kind: "done", username: outcome.username };
      const f = outcome.failure;
      switch (f.kind) {
        case "proof-required":
          // The grant was refused: ask for the proof again, once.
          auth.dropFreshProof(grant);
          continue;
        case "taken":
          return { kind: "taken" };
        case "address-mismatch":
          rememberAddress(acct.accountIdB64, null);
          return { kind: "error", text: failureText(f) };
        default:
          return { kind: "error", text: failureText(f) };
      }
    }
    return { kind: "error", text: null };
  }

  // -------------------------------------------------------------------------
  // Save
  // -------------------------------------------------------------------------

  function errorText(e: SaveError): string | null {
    switch (e.kind) {
      case "upload":
        return t("profile.save.error.upload");
      case "full":
        return t("profile.save.error.full");
      case "taken":
        return t("profile.username.taken", { username: `@${e.username}` });
      case "cancelled":
        return null;
      case "generic":
        return e.text ?? t("profile.save.error.generic");
    }
  }

  /** Section 5. Resolves whether everything was saved. */
  async function save(): Promise<boolean> {
    if (busy()) return false;
    // An open field is part of the save when valid; an invalid one blocks it.
    if (!openFieldsValid()) return false;
    leaveName();
    leaveUser();
    const before = pending();
    if (!isDirty(saved(), before)) return true;
    // ADR-0065 section 6 — the photo this save replaces or removes.
    const previousAvatar = saved().avatar;
    setBusy(true);
    setError(null);
    setNotice(null);
    try {
      const out = await runProfileSave(saved(), before, {
        uploadPhoto: async (media: LocalAvatar): Promise<AvatarData> => {
          const still = await uploadRef(media.still, media.stillSide, "image/jpeg");
          let video: AvatarData["video"] = null;
          let videoBytes: Uint8Array | null = null;
          if (media.video !== null) {
            const v = await uploadRef(media.video.blob, media.video.side, "video/mp4");
            video = { ref: v.ref, durationMs: media.video.durationMs };
            videoBytes = v.bytes;
          }
          seedAvatarBytes(still.ref.blobIdB64, still.bytes);
          if (video !== null && videoBytes !== null) seedAvatarBytes(video.ref.blobIdB64, videoBytes);
          return { still: still.ref, video };
        },
        rename,
        publish: (fields) => profiles.saveOwnProfileFields({ displayName: fields.name, avatar: fields.avatar }),
      });
      if (out.kind === "saved") {
        const photo = before.photo;
        const acct = account();
        if (photo !== undefined && previousAvatar !== null && acct !== null) {
          // The new profile is published: the previous photo's blobs go.
          const ids = [previousAvatar.still.blobIdB64, previousAvatar.video?.ref.blobIdB64]
            .filter((b): b is string => b !== undefined)
            .map((b) => blobHex(b64ToBytes(b)));
          void deleteBlobs(acct.accountIdB64, ids);
        }
        if (photo !== undefined && photo.kind !== "remove") releaseLocalAvatar(photo.media);
        setPending({});
        setNotice(t("profile.save.done"));
        if (noticeTimer !== null) clearTimeout(noticeTimer);
        noticeTimer = setTimeout(() => setNotice(null), DONE_NOTICE_MS);
        return true;
      }
      setPending(out.pending);
      setError(errorText(out.error));
      if (out.error.kind === "taken" && out.pending.username !== undefined) {
        // The username row reopens with the name that was taken.
        setUserField(openField(out.pending.username));
        queueMicrotask(() => userInput?.focus());
      }
      return false;
    } finally {
      setBusy(false);
    }
  }
  onCleanup(() => {
    if (noticeTimer !== null) clearTimeout(noticeTimer);
  });

  // -------------------------------------------------------------------------
  // Leaving (section 6)
  // -------------------------------------------------------------------------

  useBeforeLeave((e) => {
    if (e.defaultPrevented) return;
    const decision = leaveDecision(dirty(), busy());
    if (decision === "leave") return;
    e.preventDefault();
    if (decision === "ask") setLeaving(e);
  });

  async function answerLeave(answer: LeaveAnswer): Promise<void> {
    const e = leaving();
    if (e === null) return;
    const effect = leaveAnswerEffect(answer);
    if (effect.runSave) {
      const ok = await save();
      if (!ok) {
        // The person stays, with the error under the button.
        setLeaving(null);
        return;
      }
    }
    if (effect.dropPending) {
      setNameField(CLOSED_FIELD);
      setUserField(CLOSED_FIELD);
      setPhoto(undefined);
      setPending({});
    }
    setLeaving(null);
    if (effect.continueNavigation !== "never") e.retry(true);
  }

  // Closing or reloading the tab: the browser's own prompt, the one case the
  // platform leaves no other way.
  createEffect(() => {
    if (!dirty()) return;
    const onBeforeUnload = (ev: BeforeUnloadEvent): void => {
      ev.preventDefault();
    };
    window.addEventListener("beforeunload", onBeforeUnload);
    onCleanup(() => window.removeEventListener("beforeunload", onBeforeUnload));
  });

  // -------------------------------------------------------------------------
  // Markup
  // -------------------------------------------------------------------------

  const avatarLabel = (): string => shownName() ?? shownUsername() ?? "?";

  return (
    <>
      <div class="lk-page-head">
        <h1>{t("settings.section.profile")}</h1>
      </div>
      <section class="lk-profile">
        <div class="lk-profile-avatar lk-menu-anchor">
          <Avatar
            label={avatarLabel()}
            id={saved().username ?? ""}
            size="profile"
            personKey={OWN_AVATAR_KEY}
            media={mediaOfPending(pending())}
            playVideo
          />
          <button
            type="button"
            class="lk-icon-button lk-profile-avatar-edit"
            title={t("profile.avatar.change")}
            aria-label={t("profile.avatar.change")}
            aria-haspopup="menu"
            aria-expanded={menuOpen()}
            disabled={busy()}
            onClick={() => setMenuOpen((v) => !v)}
          >
            <Icon name="pencil" size={20} />
          </button>
          <Show when={menuOpen()}>
            <AvatarMenu
              canRemove={photoShown(saved(), pending())}
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
                setPhoto({ kind: "remove" });
              }}
              onUnusable={() => {
                setMenuOpen(false);
                setError(t("profile.video.unusable"));
              }}
            />
          </Show>
        </div>

        {/* Row 2: the name. */}
        <div class="lk-profile-row lk-profile-name">
          <Show
            when={nameField().open}
            fallback={
              <>
                <button
                  type="button"
                  class="lk-profile-value"
                  classList={{ muted: shownName() === null }}
                  disabled={busy()}
                  onClick={openName}
                >
                  {shownName() ?? t("profile.name.add")}
                </button>
                <button
                  type="button"
                  class="lk-icon-button"
                  title={t("profile.name.change")}
                  aria-label={t("profile.name.change")}
                  disabled={busy()}
                  onClick={openName}
                >
                  <Icon name="pencil" size={20} />
                </button>
              </>
            }
          >
            <div class="lk-profile-field">
              <input
                ref={nameInput}
                type="text"
                autocomplete="name"
                aria-label={t("profile.name.change")}
                value={nameText()}
                aria-invalid={nameCheck().kind === "invalid"}
                aria-describedby="profile-name-reason"
                disabled={busy()}
                onInput={(e) => setNameField((f) => typeIntoField(f, e.currentTarget.value))}
                onBlur={leaveName}
                onKeyDown={(e) => {
                  if (e.key === "Enter") {
                    e.preventDefault();
                    leaveName();
                  } else if (e.key === "Escape") {
                    e.preventDefault();
                    e.stopPropagation();
                    revertName();
                  }
                }}
              />
              <div id="profile-name-reason" role="status">
                <Show when={(() => {
                  const c = nameCheck();
                  return c.kind === "invalid" ? c.reason : null;
                })()}>
                  {(reason) => <div class="form-error">{reason()}</div>}
                </Show>
              </div>
            </div>
          </Show>
        </div>

        {/* Row 3: the username. */}
        <div class="lk-profile-row lk-profile-username">
          <Show
            when={userField().open}
            fallback={
              <Show
                when={shownUsername()}
                fallback={<span class="lk-profile-value muted">{t("profile.username.none")}</span>}
              >
                {(u) => (
                  <>
                    <button type="button" class="lk-profile-value" disabled={busy()} onClick={openUser}>
                      @{u()}
                    </button>
                    <button
                      type="button"
                      class="lk-icon-button"
                      title={t("profile.username.change")}
                      aria-label={t("profile.username.change")}
                      disabled={busy()}
                      onClick={openUser}
                    >
                      <Icon name="pencil" size={20} />
                    </button>
                  </>
                )}
              </Show>
            }
          >
            <div class="lk-profile-field">
              <div class="lk-profile-prefixed">
                <span aria-hidden="true">@</span>
                <input
                  ref={userInput}
                  type="text"
                  autocomplete="off"
                  autocapitalize="none"
                  spellcheck={false}
                  aria-label={t("profile.username.change")}
                  value={userText()}
                  aria-invalid={userReason() !== null}
                  aria-describedby="profile-username-reason"
                  disabled={busy()}
                  onInput={(e) => {
                    // Uppercase is shown lowercased (section 4).
                    const v = e.currentTarget.value.toLowerCase();
                    if (v !== e.currentTarget.value) e.currentTarget.value = v;
                    setUserField((f) => typeIntoField(f, v));
                  }}
                  onBlur={leaveUser}
                  onKeyDown={(e) => {
                    if (e.key === "Enter") {
                      e.preventDefault();
                      leaveUser();
                    } else if (e.key === "Escape") {
                      e.preventDefault();
                      e.stopPropagation();
                      revertUser();
                    }
                  }}
                />
              </div>
              <div id="profile-username-reason" role="status">
                <Show when={userReason()}>{(reason) => <div class="form-error">{reason()}</div>}</Show>
              </div>
            </div>
          </Show>
        </div>

        <Show when={asksRenameAddress()}>
          <div class="lk-profile-field">
            <label>
              <span class="lk-muted">{t("protect.username_email")}</span>
              <input
                type="email"
                autocomplete="email"
                inputmode="email"
                dir="ltr"
                aria-label={t("recovery_email.field")}
                placeholder={t("recovery_email.field")}
                value={renameAddress()}
                disabled={busy()}
                onInput={(e) => setRenameAddress(e.currentTarget.value)}
              />
            </label>
          </div>
        </Show>

        <div class="lk-profile-save">
          <button
            type="button"
            class="primary"
            disabled={!canSave() || (asksRenameAddress() && !/^[^@\s]+@[^@\s]+$/.test(renameAddress().trim()))}
            onMouseDown={(e) => {
              // Keep an open field's blur from running first: the save folds
              // it in itself.
              e.preventDefault();
            }}
            onClick={() => void save()}
          >
            {busy() ? t("profile.save.saving") : t("common.save")}
          </button>
          <Show when={error()}>
            {(msg) => (
              <div class="form-error" role="alert">
                {msg()}
              </div>
            )}
          </Show>
          <Show when={notice()}>
            {(msg) => (
              <div class="form-info" role="status">
                {msg()}
              </div>
            )}
          </Show>
        </div>
      </section>

      <Show when={cropSource()}>
        {(source) => (
          <AvatarCrop
            source={source()}
            onCancel={() => setCropSource(null)}
            onUse={(local) => {
              setCropSource(null);
              setError(null);
              setPhoto({ kind: "set", media: local });
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
      <ModalLayer open={proofOpen()} title={t("username.change.title")} onClose={() => answerProof(null)}>
        <ProofBlock gate={gate} />
        <div class="lk-actions">
          <button
            type="button"
            class="primary"
            disabled={proofChecking()}
            onClick={() => void continueProof()}
          >
            {proofChecking() ? t("common.saving") : t("common.save")}
          </button>
          <button type="button" class="ghost" disabled={proofChecking()} onClick={() => answerProof(null)}>
            {t("common.cancel")}
          </button>
        </div>
      </ModalLayer>
      <UnsavedChangesDialog
        open={leaving() !== null}
        busy={busy()}
        onSave={() => void answerLeave("save")}
        onDiscard={() => void answerLeave("discard")}
        onCancel={() => void answerLeave("cancel")}
      />
    </>
  );
}
