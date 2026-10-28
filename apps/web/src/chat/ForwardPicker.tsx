// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The "Forward to" picker, ADR-0052 decision 2 /
 * `docs/ux/message-actions.md` section 5.1.
 *
 * A layer over the chat (`ModalLayer`, the same idiom the conversation info
 * uses) holding the same merged list the Chats tab shows - one-to-one chats
 * and groups this device is a live member of, pinned first, newest next,
 * with the search field - one row per conversation, no multi-select. Picking
 * a row sends at once: the row shows a busy state while the attachments are
 * re-encrypted and the message is published, and on failure the picker stays
 * open with the error in place (the layer's own convention). Back, `Esc` and
 * the backdrop cancel and send nothing.
 *
 * Which rows appear and in what order is `forward-picker.ts`, where a test can
 * run it; this component computes one candidate per active group from the
 * same contexts the Chats list reads (the title through `conversationTitle`,
 * the second line and the time from the persisted message log, the flags
 * from the sync runtime) and draws what the rule returns.
 */
import { createEffect, createMemo, createSignal, For, Show, type JSX } from "solid-js";

import { useAuth } from "../auth/AuthContext";
import { getDeviceKey } from "../auth/key-store";
import {
  loadAccepted,
  peerContactByMemberPubkeys,
  type AcceptedContact,
} from "../contacts/local-store";
import { groupIdHex as syncGroupIdHex } from "../devices/sync/state";
import { useDeviceGroupSync } from "../devices/sync/DeviceGroupSyncContext";
import { useLineage } from "./use-lineage";
import { memberNameOf } from "../profile/member-label";
import { useProfiles } from "../profile/ProfileContext";
import { Avatar } from "../ui/Avatar";
import { Icon } from "../ui/icons";
import { ModalLayer } from "../ui/ModalLayer";
import { summarizeGroupSlot } from "./background-log";
import { useChats, type ActiveGroup } from "./ChatContext";
import { conversationKindOf } from "./conversation-kind";
import { conversationTitle } from "./conversation-title";
import {
  filterForwardRows,
  forwardPickerRows,
  type ForwardCandidate,
  type ForwardPickerRow,
} from "./forward-picker";
import { useGroupNames } from "./GroupNameContext";
import { groupTitle } from "./group-title";
import { previewLine } from "./list-preview";
import { readIsActive } from "./membership-ended";
import { loadMessageLog, type PersistedGroup } from "./message-log-store";
import { ownPersonKeyB64, personKeysOf } from "./person-key";
import { uiRules } from "./ui-rules";
import { t } from "../l10n";
import { keyedErrorText } from "../l10n/keyed-error";
import { createLiveText } from "../l10n/live-text";

export interface ForwardPickerProps {
  readonly open: boolean;
  readonly onClose: () => void;
  /** Send to the picked conversation. Resolves once the message is
   *  published; a rejection keeps the picker open with the error shown. */
  readonly onPick: (row: ForwardPickerRow) => Promise<void>;
}

/** Section 5.1 "Copy of the day": the picker's title is "Forward to". */
export const FORWARD_PICKER_TITLE = (): string => t("chat.forward.picker_title");

export function ForwardPicker(props: ForwardPickerProps): JSX.Element {
  const chats = useChats();
  const profiles = useProfiles();
  const groupNames = useGroupNames();
  const sync = useDeviceGroupSync();
  // ADR-0073 R.5 — one candidate per conversation: its current group, with
  // the conversation's own pin / mute / archive.
  const lineageView = useLineage();
  const auth = useAuth();

  const accountId = (): string | null => {
    const s = auth.state();
    return s.kind === "session" ? s.account.accountId : null;
  };

  // The persisted message log, for the second line and the time: loaded when
  // the picker opens, not on every log write — the picker is up for seconds.
  const [logSlots, setLogSlots] = createSignal<{ readonly [groupIdHex: string]: PersistedGroup }>(
    {},
  );
  const [accepted, setAccepted] = createSignal<AcceptedContact[]>([]);
  const [query, setQuery] = createSignal("");
  const [busyId, setBusyId] = createSignal<string | null>(null);
  const [error, setError] = createLiveText();

  createEffect(() => {
    if (!props.open) return;
    setQuery("");
    setBusyId(null);
    setError(null);
    setAccepted(loadAccepted());
    void (async (): Promise<void> => {
      const account = accountId();
      if (account === null) return;
      const deviceKey = await getDeviceKey(account);
      if (deviceKey === null) return;
      const log = await loadMessageLog(account, deviceKey);
      if (log !== null) setLogSlots(log.groups);
    })();
  });

  /** The row's name, exactly as the Chats list names it. */
  const titleOf = (g: ActiveGroup, kind: "chat" | "group"): string => {
    const ownB64 = ownPersonKeyB64(chats.identity());
    const memberB64s = personKeysOf(g.group);
    return conversationTitle({
      kind,
      memberKeysB64: memberB64s,
      ownKeyB64: ownB64,
      fallbackLabel: g.label,
      sharedName: groupNames.nameOf(g.groupIdB64),
      contactNameById: accepted().find((c) => c.groupIdB64 === g.groupIdB64)?.displayName,
      contactNameByMember: peerContactByMemberPubkeys(memberB64s, ownB64, accepted())
        ?.displayName,
      nameOf: (k) => profiles.resolve(k).name,
      groupTitle,
    });
  };

  const candidates = createMemo((): readonly ForwardCandidate[] => {
    const flagsMap = sync.flags();
    return lineageView.conversations().map((c) => {
      const g = c.head;
      const hex = syncGroupIdHex(g.group.groupId);
      const kind = conversationKindOf(g.group);
      const summary = summarizeGroupSlot(logSlots()[hex], null);
      const line = previewLine({
        preview: summary.preview,
        isOwn: summary.previewIsOwn,
        senderName:
          summary.lastAuthorKeyB64 === null
            ? null
            : memberNameOf(profiles.resolve(summary.lastAuthorKeyB64)),
        isGroup: kind === "group",
      });
      const lastAt = summary.lastAt === null ? Number.NaN : new Date(summary.lastAt).getTime();
      const f = flagsMap[c.conversationHex];
      return {
        groupIdB64: g.groupIdB64,
        kind,
        title: titleOf(g, kind),
        sub: line,
        lastActivityMs: Number.isFinite(lastAt) ? lastAt : g.createdAtMs,
        pinned: f?.pinned === true,
        muted: f?.muted === true,
        archived: f?.archived === true,
        live: readIsActive(g.group),
        ...(kind === "chat"
          ? { personKey: personKeysOf(g.group).find((k) => k !== ownPersonKeyB64(chats.identity())) }
          : {}),
      };
    });
  });

  const rows = createMemo((): readonly ForwardPickerRow[] => {
    const rules = uiRules();
    if (rules === null) return [];
    return forwardPickerRows(candidates(), rules);
  });
  const visible = (): readonly ForwardPickerRow[] => filterForwardRows(rows(), query());

  async function pick(row: ForwardPickerRow): Promise<void> {
    if (busyId() !== null) return;
    setError(null);
    setBusyId(row.id);
    try {
      await props.onPick(row);
    } catch (err) {
      // ADR-0061 section 6: the error's code, never its prose.
      console.warn("[forward] failed", err);
      setError(() => keyedErrorText(err) ?? t("chat.forward.error.generic"));
    } finally {
      setBusyId(null);
    }
  }

  return (
    <ModalLayer
      open={props.open}
      title={FORWARD_PICKER_TITLE()}
      onClose={() => {
        // Cancel sends nothing - but a send already in flight is not
        // cancelled by closing over it; the layer simply goes.
        props.onClose();
      }}
    >
      <Show when={rows().length > 0}>
        <label class="lk-search lk-forward-search">
          <Icon name="search" size={16} />
          <input
            type="search"
            placeholder={t("chats.search.placeholder")}
            aria-label={t("chat.forward.search_label")}
            value={query()}
            onInput={(e) => setQuery(e.currentTarget.value)}
          />
        </label>
      </Show>
      <Show when={error()}>{(message) => <div class="form-error">{message()}</div>}</Show>
      <Show
        when={rows().length > 0}
        fallback={<p class="lk-pane-nomatch">{t("chat.forward.empty")}</p>}
      >
        <Show when={visible().length === 0}>
          <p class="lk-pane-nomatch">{t("chats.search.no_results")}</p>
        </Show>
        <ul class="lk-forward-list" aria-label={t("chat.forward.list_label")}>
          <For each={visible()}>
            {(row) => {
              const busy = (): boolean => busyId() === row.id;
              return (
                <li>
                  <button
                    type="button"
                    class="lk-row lk-forward-row"
                    aria-busy={busy()}
                    disabled={busyId() !== null}
                    onClick={() => void pick(row)}
                  >
                    <Avatar label={row.title} id={row.id} personKey={row.personKey} />
                    <div class="lk-row-body">
                      <div class="lk-row-line">
                        <span class="lk-row-title">
                          <span class="lk-ellipsis">{row.title}</span>
                        </span>
                        <Show when={row.pinned}>
                          <Icon name="pin" size={14} title={t("chats.row.pinned")} />
                        </Show>
                      </div>
                      <div class="lk-row-line">
                        <span class="lk-row-sub">
                          {busy() ? t("common.sending") : row.preview}
                        </span>
                      </div>
                    </div>
                  </button>
                </li>
              );
            }}
          </For>
        </ul>
      </Show>
    </ModalLayer>
  );
}
