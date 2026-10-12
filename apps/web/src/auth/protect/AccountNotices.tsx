// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * What the account's own devices are told (docs/ux/identity-recovery.md
 * sections 19.2, 19.5 and 19.6, ADR-0067 section 9), and the per-landing
 * read of how the account is protected.
 *
 * At every session landing this reads `GET /recovery-email` (which keeps
 * the "phrase-protected" flag of this device in step) and
 * `GET /account/notices` from the device's seen cursor. Each notice is shown
 * once, in the app's own dialog, with `It was me` / `It was not me`; the
 * latter opens one screen: a new password (this device holds the keys, so it
 * asks nothing else for ten minutes), `Remove` (behind the proof block),
 * and, when no phrase is saved, `Save a recovery phrase`. A device is not
 * told about its own action (one notice per marker, within ten minutes). A
 * device with no cursor yet asks for the last thirty days.
 *
 * Whole-session, beside `OwedKeyRotation`. Nothing is shown while a guard
 * step is up (new words, the protect step) or the device is signed out;
 * when the keys were reset after the change the "Was it you?" screen of
 * section 18.4 is what the device shows instead.
 */
import { plainStore } from "./local";
import { createEffect, createSignal, For, onCleanup, Show, type JSX } from "solid-js";

import { useAuth } from "../AuthContext";
import { NewPasswordForm } from "../NewPasswordForm";
import { PasswordChangeGate } from "../PasswordChangeGate";
import { currentLanguage, t } from "../../l10n";
import { ModalLayer } from "../../ui/ModalLayer";
import {
  consumeSelfMarks,
  firstNoticeQuery,
  noticeCursorKnown,
  noticePasswordOpen,
  noticeView,
  planNotices,
  readNoticeCursor,
  readSelfMarks,
  secureActions,
  startNoticeCursor,
  writeNoticeCursor,
  type AccountNotice,
  type SecureAction,
} from "./notices";
import { protectionStatus, refreshProtection } from "./protection-state";
import { RecoveryEmailClient } from "./recovery-email-client";
import { RemoveRecoveryEmail } from "./RemoveRecoveryEmail";
import { SavePhrasePanel } from "./SavePhrasePanel";

/** At most this many pages are read in one landing. */
const MAX_PAGES = 5;

function mediumDate(ms: number): string {
  return new Date(ms).toLocaleDateString(currentLanguage(), { dateStyle: "medium" });
}

export function AccountNotices(): JSX.Element {
  const auth = useAuth();
  const [queue, setQueue] = createSignal<AccountNotice[]>([]);
  const [securing, setSecuring] = createSignal(false);
  // When the secure screen was opened from a notice, and a clock for the
  // ten minutes its password change stays open without another proof.
  const [securingSinceMs, setSecuringSinceMs] = createSignal<number | null>(null);
  const [nowMs, setNowMs] = createSignal(Date.now());
  const clock = setInterval(() => setNowMs(Date.now()), 1_000);
  onCleanup(() => clearInterval(clock));
  const passwordOpen = (): boolean => noticePasswordOpen(securingSinceMs(), nowMs());
  const [passwordSaved, setPasswordSaved] = createSignal(false);
  const [removing, setRemoving] = createSignal(false);

  const landed = (): { accountIdB64: string; token: string; generation: string } | null => {
    const s = auth.state();
    const secrets = auth.identitySecrets();
    if (s.kind !== "session" || secrets === null || auth.signedOut() !== null) return null;
    return {
      accountIdB64: s.account.accountId,
      token: secrets.sessionToken,
      generation: secrets.keyGeneration.toString(),
    };
  };
  /** Nothing of this component is on screen while a step that comes before
   *  every route is up. */
  const quiet = (): boolean =>
    landed() === null || auth.signupPhrase() !== null || auth.protectStepOwed();

  async function read(accountIdB64: string, token: string): Promise<void> {
    await refreshProtection(accountIdB64, token);
    const client = new RecoveryEmailClient();
    const cursor = readNoticeCursor(plainStore(), accountIdB64);
    const all: AccountNotice[] = [];
    // From the cursor; a device that has none asks for the last 30 days.
    let query = firstNoticeQuery(noticeCursorKnown(plainStore(), accountIdB64), cursor, Date.now());
    let nextAfter = cursor;
    try {
      for (let page = 0; page < MAX_PAGES; page += 1) {
        const got = await client.notices(token, query);
        all.push(...got.notices);
        const advanced = got.nextAfter > nextAfter;
        nextAfter = Math.max(nextAfter, got.nextAfter);
        if (!got.more || !advanced) break;
        query = { kind: "after", after: nextAfter };
      }
    } catch {
      // Offline or a server error: asked again at the next landing.
      return;
    }
    const plan = planNotices({
      cursor,
      notices: all,
      nextAfter,
      marks: readSelfMarks(plainStore(), accountIdB64),
    });
    // The device keeps a cursor from its first answered read on.
    startNoticeCursor(plainStore(), accountIdB64, plan.cursor);
    writeNoticeCursor(plainStore(), accountIdB64, plan.cursor);
    // One notice per marker.
    consumeSelfMarks(plainStore(), accountIdB64, plan.usedMarks);
    // A phrase was saved on another device: this one is phrase-protected
    // from now on (the three-word surfaces, `Show recovery phrase`).
    if (all.some((n) => n.kind === "recovery-phrase-saved")) {
      await refreshProtection(accountIdB64, token);
    }
    // The account may have changed while the requests were out.
    if (landed()?.accountIdB64 === accountIdB64) setQueue(plan.show);
  }

  // One read per landing, and again when the account's keys changed under
  // this session (a key reset here, a rotation adopted from another device,
  // a phrase saved): that is when the mode moves. A token refresh is not a
  // landing.
  let readFor: string | null = null;
  createEffect(() => {
    const l = landed();
    if (l === null) {
      readFor = null;
      setQueue([]);
      setSecuring(false);
      return;
    }
    const key = `${l.accountIdB64}:${l.generation}`;
    if (readFor === key) return;
    readFor = key;
    void read(l.accountIdB64, l.token);
  });

  const current = (): AccountNotice | null => (quiet() ? null : (queue()[0] ?? null));

  /** The notice was answered: it is not shown again on this device. */
  function seen(notice: AccountNotice): void {
    const l = landed();
    if (l !== null) writeNoticeCursor(plainStore(), l.accountIdB64, notice.id);
    setQueue((q) => q.filter((n) => n.id !== notice.id));
  }
  function onNotMe(notice: AccountNotice): void {
    seen(notice);
    setPasswordSaved(false);
    setRemoving(false);
    setNowMs(Date.now());
    setSecuringSinceMs(Date.now());
    setSecuring(true);
  }

  const actions = (): SecureAction[] => {
    const l = landed();
    const st = protectionStatus(l?.accountIdB64 ?? null);
    return secureActions({
      canReplacePassword: auth.canReplacePassword(),
      emailEnabled: st?.enabled === true,
      phraseSaved: st?.phraseSaved === true,
    });
  };

  return (
    <>
      <Show when={!securing() ? current() : null}>
        {(notice) => {
          const view = (): ReturnType<typeof noticeView> => noticeView(notice().kind);
          const title = (): string => {
            const key = view().titleKey;
            return key === null ? t(view().bodyKey) : t(key);
          };
          return (
            <ModalLayer open={true} title={title()} onClose={() => seen(notice())}>
              <div class="lk-stack">
                <Show when={view().titleKey !== null}>
                  <p>
                    {view().dated
                      ? t(view().bodyKey, { date: mediumDate(notice().atMs) })
                      : t(view().bodyKey)}
                  </p>
                </Show>
                <div class="lk-actions">
                  <button type="button" class="primary" onClick={() => seen(notice())}>
                    {t("notice.was_me")}
                  </button>
                  <button type="button" onClick={() => onNotMe(notice())}>
                    {t("notice.was_not_me")}
                  </button>
                </div>
              </div>
            </ModalLayer>
          );
        }}
      </Show>
      <ModalLayer
        open={securing() && !quiet()}
        title={t("notice.secure.title")}
        onClose={() => setSecuring(false)}
      >
        <Show when={securing() && !quiet()}>
          <div class="lk-stack">
            <For each={actions()}>
              {(action) => (
                <section class="lk-stack">
                  <Show when={action === "new-password"}>
                    <h3>{t("recovery.new_password.title")}</h3>
                    <Show
                      when={!passwordSaved()}
                      fallback={
                        <p class="form-info" role="status">
                          {t("notice.secure.password_saved")}
                        </p>
                      }
                    >
                      {/* Open for ten minutes from the notice; after that
                          the ordinary proof comes first. */}
                      <Show
                        when={passwordOpen()}
                        fallback={
                          <PasswordChangeGate>
                            <NewPasswordForm onSaved={() => setPasswordSaved(true)} />
                          </PasswordChangeGate>
                        }
                      >
                        <NewPasswordForm afterAccountNotice onSaved={() => setPasswordSaved(true)} />
                      </Show>
                    </Show>
                  </Show>
                  <Show when={action === "remove-email"}>
                    <h3>{t("recovery_email.title")}</h3>
                    <Show
                      when={removing()}
                      fallback={
                        <button type="button" class="danger" onClick={() => setRemoving(true)}>
                          {t("recovery_email.remove")}
                        </button>
                      }
                    >
                      <RemoveRecoveryEmail
                        onRemoved={() => setRemoving(false)}
                        onCancel={() => setRemoving(false)}
                      />
                    </Show>
                  </Show>
                  <Show when={action === "save-phrase"}>
                    <h3>{t("protect.phrase.title")}</h3>
                    <SavePhrasePanel
                      extraLine={t("notice.secure.new_keys")}
                      onSaved={() => setSecuring(false)}
                    />
                  </Show>
                </section>
              )}
            </For>
            <div class="lk-actions">
              <button type="button" onClick={() => setSecuring(false)}>
                {t("common.done")}
              </button>
            </div>
          </div>
        </Show>
      </ModalLayer>
    </>
  );
}
