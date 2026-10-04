// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0065 phase 2 (UX 16.5) — the Recreate group sheet: the new-group flow
 * (`routes/NewGroup.tsx`) pre-filled, as a layer over the old group.
 *
 * `Create and close the old group` is one action: found the new group with
 * the first ticked member (the creating admin becomes its owner), record the
 * name, post the group-closed notice into the OLD group naming the new one,
 * hand the remaining members to the chat view's own sequential add path
 * (`pending-group-setup.ts`, the same hand-off the new-group flow uses, with
 * its obligations: durable commit record, proposal + sweep, delegations,
 * push-routing redistribution), then open the new group.
 *
 * The rows and their pre-set state are `recreate-group.ts`; this renders
 * them and runs the sequence.
 */
import { createEffect, createMemo, createSignal, For, Show, on, type JSX } from "solid-js";
import { useNavigate } from "@solidjs/router";

import { useAuth } from "../auth/AuthContext";
import { base64Decode, base64UrlEncodeNoPad } from "../auth/account-id";
import type { ContactsClient } from "../contacts/client";
import { contactsErrorText } from "../contacts/errors";
import { joinList, t } from "../l10n";
import { createLiveText } from "../l10n/live-text";
import { useProfiles } from "../profile/ProfileContext";
import { paths } from "../routes/paths";
import { Avatar } from "../ui/Avatar";
import { ModalLayer } from "../ui/ModalLayer";
import { useChats, type ActiveGroup } from "./ChatContext";
import { useGroupClosed } from "./GroupClosedContext";
import { useGroupNames } from "./GroupNameContext";
import { DEFAULT_GROUP_LABEL } from "./group-title";
import { accountIdBytesFromHex, setPendingGroupSetup } from "./pending-group-setup";
import {
  initialTicks,
  recreateInvitees,
  recreateRows,
  recreateSubmitEnabled,
  type RecreateMember,
  type RecreateRow,
} from "./recreate-group";
import {
  RECREATE_IDLE,
  recreateProgressReduce,
  recreateProgressView,
  type RecreateProgress,
  type RecreateProgressEvent,
} from "./recreate-progress";

const [recreateProgress, setRecreateProgress] = createSignal<RecreateProgress>(RECREATE_IDLE);

/** Feed the sheet's progress (`recreate-progress.ts`). The chat view calls
 *  this with `adds-finished` once the parked invitations ran. */
export function dispatchRecreateProgress(event: RecreateProgressEvent): void {
  setRecreateProgress((s) => recreateProgressReduce(s, event));
}

/**
 * The Recreate group sheet after the new group was opened: mounted by the
 * chat view of every group, visible only over the group being recreated.
 */
export function RecreateProgressLayer(props: { readonly groupIdB64: string }): JSX.Element {
  const view = createMemo(() => recreateProgressView(recreateProgress(), props.groupIdB64));
  const dismiss = (): void => dispatchRecreateProgress({ kind: "dismissed" });
  return (
    <ModalLayer open={view().open} title={t("group.recreate.title")} onClose={dismiss}>
      <div class="lk-stack">
        <Show when={view().failedNames}>
          {(names) => (
            <div class="form-error" role="alert">
              {t("chat.members.add.error.some_failed", { names: joinList(names()) })}
            </div>
          )}
        </Show>
        <div class="lk-actions">
          <Show
            when={!view().working}
            fallback={
              <button type="button" class="primary" disabled aria-busy="true">
                {t("group.recreate.working")}
              </button>
            }
          >
            <button type="button" class="primary" onClick={dismiss}>
              {t("group.recreate.done")}
            </button>
          </Show>
        </div>
      </div>
    </ModalLayer>
  );
}

/** The 64-character cap the new-group flow applies. */
const GROUP_NAME_MAX = 64;

export interface RecreateGroupSheetProps {
  readonly open: boolean;
  readonly onClose: () => void;
  /** The OLD group. */
  readonly entry: ActiveGroup;
  /** The old group's current title, pre-filled as the name. */
  readonly currentName: string;
  readonly contacts: ContactsClient;
  readonly roster: readonly RecreateMember[];
  readonly ownPersonKeyB64: string | null;
  /** The reset person's OLD key (`previousKey`). */
  readonly oldOwnerKeyB64: string;
  /** The reset person's NEW identity as this device knows it, or `null`. */
  readonly resetContact: {
    readonly label: string;
    readonly accountIdHex: string;
    readonly signaturePubkeyB64: string;
    readonly unverified: boolean;
  } | null;
}

export function RecreateGroupSheet(props: RecreateGroupSheetProps): JSX.Element {
  const auth = useAuth();
  const chats = useChats();
  const profiles = useProfiles();
  const groupNames = useGroupNames();
  const groupClosed = useGroupClosed();
  const navigate = useNavigate();

  const [name, setName] = createSignal("");
  const [ticked, setTicked] = createSignal<Set<string>>(new Set());
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createLiveText();

  const rows = createMemo<RecreateRow[]>(() =>
    recreateRows({
      roster: props.roster,
      ownPersonKeyB64: props.ownPersonKeyB64,
      oldOwnerKeyB64: props.oldOwnerKeyB64,
      resetContact: props.resetContact,
    }),
  );

  // Pre-fill on every open: the name and the ticks are the group's, not the
  // sheet's last state.
  createEffect(
    on(
      () => props.open,
      (open) => {
        if (!open) return;
        setName(props.currentName.slice(0, GROUP_NAME_MAX));
        setTicked(initialTicks(rows()));
        setError(null);
        setBusy(false);
      },
    ),
  );

  const toggle = (key: string): void => {
    const next = new Set(ticked());
    if (next.has(key)) next.delete(key);
    else next.add(key);
    setTicked(next);
  };

  const submitEnabled = (): boolean => recreateSubmitEnabled(rows(), ticked());

  async function onSubmit(e: SubmitEvent): Promise<void> {
    e.preventDefault();
    if (busy() || !submitEnabled()) return;
    setError(null);
    const secrets = auth.identitySecrets();
    if (secrets === null) {
      setError(() => t("new_group.error.sign_in"));
      return;
    }
    const [first, ...rest] = recreateInvitees(rows(), ticked());
    if (first === undefined) return;
    setBusy(true);
    try {
      const result = await props.contacts.startGroupWith(
        secrets,
        accountIdBytesFromHex(first.accountIdHex),
        base64Decode(first.signaturePubkeyB64),
        "group",
      );
      const label = name().trim().length > 0 ? name().trim() : DEFAULT_GROUP_LABEL;
      const active = chats.add({ label, group: result.group });
      profiles.groupJoined(active.groupIdB64);
      if (name().trim().length > 0) {
        try {
          await groupNames.recordCreated(active, name());
        } catch (err) {
          // eslint-disable-next-line no-console
          console.warn("recreate group: name not recorded", err);
        }
      }
      const s = auth.state();
      if (s.kind === "session") await chats.persistSnapshot(s.account.accountId);
      // UX 16.5 — the closed notice into the OLD group, naming the new one.
      // The new group exists at this point whatever happens here: a failed
      // post keeps the sheet open with the error, and this device's own view
      // of the old group is already closed (the store holds it).
      await groupClosed.close(props.entry, result.group.groupId);
      if (rest.length > 0) {
        setPendingGroupSetup({
          groupIdB64: active.groupIdB64,
          members: rest.map((m) => ({
            accountIdHex: m.accountIdHex,
            signaturePubkeyB64: m.signaturePubkeyB64,
            label: m.label,
          })),
          reportTo: "recreate-sheet",
        });
        // The sheet carries on over the new group until they ran.
        dispatchRecreateProgress({ kind: "handed-off", groupIdB64: active.groupIdB64 });
      }
      props.onClose();
      navigate(paths.groupChat(base64UrlEncodeNoPad(result.group.groupId)));
    } catch (err) {
      setError(contactsErrorText(err, "chats.new_group.error"));
      setBusy(false);
    }
  }

  return (
    <ModalLayer
      open={props.open}
      title={t("group.recreate.title")}
      onClose={() => {
        // UX 16.5 — the sheet cannot be dismissed while working.
        if (!busy()) props.onClose();
      }}
    >
      <form class="lk-stack" onSubmit={(e) => void onSubmit(e)}>
        <section class="lk-card">
          <label>
            {t("chat.group_name.label")}
            <input
              type="text"
              maxLength={GROUP_NAME_MAX}
              value={name()}
              disabled={busy()}
              onInput={(e) => setName(e.currentTarget.value)}
            />
          </label>
        </section>
        <section class="lk-card" style={{ gap: "6px" }}>
          <h2>{t("group.recreate.members_heading", { count: rows().length })}</h2>
          <For each={rows()}>
            {(row) => (
              <label class="lk-row pick" style={{ opacity: row.enabled ? 1 : 0.55 }}>
                <input
                  type="checkbox"
                  checked={ticked().has(row.personKeyB64)}
                  disabled={busy() || !row.enabled}
                  onChange={() => toggle(row.personKeyB64)}
                />
                <Avatar label={row.label} id={row.personKeyB64} />
                <div class="lk-row-body">
                  <span class="lk-row-title">{row.label}</span>
                  <Show when={row.reason === "keys-reset-unverified"}>
                    <span class="lk-row-sub">{t("group.recreate.excluded_reset")}</span>
                  </Show>
                  <Show when={row.reason === "no-account-id"}>
                    <span class="lk-row-sub">{t("chats.new_group.accept_again")}</span>
                  </Show>
                </div>
              </label>
            )}
          </For>
        </section>
        <p style={{ "font-size": "0.85rem", color: "var(--surface-muted)", margin: 0 }}>
          {t("group.recreate.note")}
        </p>
        <Show when={error()}>{(m) => <div class="form-error">{m()}</div>}</Show>
        <div class="lk-actions">
          <button type="submit" class="primary" disabled={busy() || !submitEnabled()}>
            {busy() ? t("group.recreate.working") : t("group.recreate.submit")}
          </button>
          <button type="button" class="ghost" disabled={busy()} onClick={() => props.onClose()}>
            {t("common.cancel")}
          </button>
        </div>
      </form>
    </ModalLayer>
  );
}
