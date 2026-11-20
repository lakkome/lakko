// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * "Your account is suspended" (ADR-0098 step 3, UX
 * `reporting-and-moderation.md` section 4): the one screen that says why.
 *
 * Replaces every route while this device is in suspended mode and the person
 * has not chosen "Read my chats". Drawn in the auth screens' frame, like the
 * other screens that stand in for the app. The details come from `GET
 * /account/suspension` with the restricted session; Appeal opens the appeal
 * sheet, sent once per suspension; Sign out signs this device out.
 *
 * On a device that holds none of the account's chats there is nothing to
 * read and Settings cannot be reached, so "Delete account" takes the place of
 * "Read my chats" and opens the usual delete-account confirmation, run with
 * the restricted session.
 */

import { createSignal, For, Match, onMount, Show, Switch, type JSX } from "solid-js";
import { useNavigate } from "@solidjs/router";

import { useAuth } from "./AuthContext";
import {
  APPEAL_MAX_CHARS,
  appealFailureKey,
  appealLength,
  appealLineKey,
  appealSendable,
  reasonCatalogKey,
  suspendedScreenActions,
  type AppealState,
  type SuspensionDetails,
} from "./suspension";
import { paths } from "../routes/paths";
import { AuthLayout } from "../ui/AuthLayout";
import { ModalLayer } from "../ui/ModalLayer";
import { t } from "../l10n";
import { displayLocale } from "../l10n/display-locale";
import { DeleteAccountDialog } from "./AccountSections";
import { base64Decode } from "./account-id";

type Load =
  | { readonly kind: "loading" }
  | { readonly kind: "failed" }
  | { readonly kind: "ready"; readonly details: SuspensionDetails };

export function SuspendedScreen(): JSX.Element {
  const auth = useAuth();
  const navigate = useNavigate();
  const [load, setLoad] = createSignal<Load>({ kind: "loading" });
  const [appealOpen, setAppealOpen] = createSignal(false);
  const [signingOut, setSigningOut] = createSignal(false);
  const [deleting, setDeleting] = createSignal(false);

  async function refreshDetails(): Promise<void> {
    setLoad({ kind: "loading" });
    const status = await auth.loadSuspension();
    if (status === null) {
      setLoad({ kind: "failed" });
      return;
    }
    // `{ suspended: false }`: the suspension ended; suspended mode is left
    // by the refresh `loadSuspension` started, and this screen goes with it.
    if (status.suspended) setLoad({ kind: "ready", details: status.details });
  }
  onMount(() => {
    void refreshDetails();
  });

  const canRead = (): boolean => auth.suspension()?.canRead === true;
  const ready = (): SuspensionDetails | null => {
    const l = load();
    return l.kind === "ready" ? l.details : null;
  };
  const appeal = (): AppealState | null => ready()?.appeal ?? null;

  const untilLine = (details: SuspensionDetails): string =>
    details.untilMs === null
      ? t("suspended.self.no_end")
      : t("suspended.self.until", {
          date: new Date(details.untilMs).toLocaleString(displayLocale(), {
            dateStyle: "long",
            timeStyle: "short",
          }),
        });

  function signOut(): void {
    if (signingOut()) return;
    setSigningOut(true);
    auth.logout();
    navigate(paths.login, { replace: true });
  }

  /** What the delete-account confirmation deletes from this device: the
   *  suspended account, with its restricted session. */
  const deleteTarget = () => {
    const mode = auth.suspension();
    const bearer = auth.suspendedBearer();
    if (mode === null || bearer === null) return null;
    let accountId: Uint8Array;
    try {
      accountId = base64Decode(mode.accountIdB64);
    } catch {
      return null;
    }
    return {
      accountId,
      bearer,
      usernameKnown: mode.username.trim().length > 0,
      finish: async (): Promise<void> => {
        await auth.finishSuspendedDeletion();
        navigate(paths.login, { replace: true });
      },
    };
  };

  function appealSent(): void {
    setAppealOpen(false);
    const l = load();
    if (l.kind === "ready") setLoad({ kind: "ready", details: { ...l.details, appeal: "sent" } });
  }

  return (
    <AuthLayout>
      <h1>{t("suspended.self.title")}</h1>
      <Switch>
        <Match when={load().kind === "loading"}>
          <p class="form-info" role="status" aria-live="polite">
            {t("suspended.self.loading")}
          </p>
        </Match>
        <Match when={load().kind === "failed"}>
          <p class="form-error" role="alert">
            {t("suspended.self.load_failed")}
          </p>
          <button type="button" class="secondary" onClick={() => void refreshDetails()}>
            {t("common.retry")}
          </button>
        </Match>
        <Match when={ready()}>
          {(details) => (
            <>
              <p class="lk-auth-lede">
                {t("common.sentence_pair", {
                  first: t("suspended.self.reason", { reason: t(reasonCatalogKey(details().reasonName)) }),
                  second: untilLine(details()),
                })}
              </p>
              <p class="lk-muted">{t("suspended.self.body")}</p>
            </>
          )}
        </Match>
      </Switch>
      <Show when={appeal() !== null ? appealLineKey(appeal() ?? "none") : null}>
        {(key) => (
          <p class="form-info" role="status">
            {t(key())}
          </p>
        )}
      </Show>
      <div class="lk-stack lk-suspended-actions">
        <For each={suspendedScreenActions(canRead(), appeal())}>
          {(action) => (
            <Switch>
              <Match when={action === "appeal"}>
                <button
                  type="button"
                  class={canRead() ? "secondary" : "primary"}
                  onClick={() => setAppealOpen(true)}
                >
                  {t("suspended.appeal")}
                </button>
              </Match>
              <Match when={action === "read"}>
                <button type="button" class="primary" onClick={() => auth.readSuspendedChats()}>
                  {t("suspended.self.read")}
                </button>
              </Match>
              <Match when={action === "delete"}>
                <button type="button" class="danger" onClick={() => setDeleting(true)}>
                  {t("suspended.self.delete")}
                </button>
              </Match>
              <Match when={action === "sign-out"}>
                <button type="button" class="ghost" disabled={signingOut()} onClick={signOut}>
                  {t("suspended.self.sign_out")}
                </button>
              </Match>
            </Switch>
          )}
        </For>
      </div>
      <AppealSheet open={appealOpen()} onClose={() => setAppealOpen(false)} onSent={appealSent} />
      <Show when={deleting() ? deleteTarget() : null}>
        {(target) => (
          <DeleteAccountDialog
            target={target()}
            onClose={() => setDeleting(false)}
          />
        )}
      </Show>
    </AuthLayout>
  );
}

/** The appeal sheet: one field, sent once. */
function AppealSheet(props: {
  readonly open: boolean;
  readonly onClose: () => void;
  readonly onSent: () => void;
}): JSX.Element {
  const auth = useAuth();
  const [text, setText] = createSignal("");
  const [sending, setSending] = createSignal(false);
  const [failure, setFailure] = createSignal<string | null>(null);

  async function send(): Promise<void> {
    if (sending() || !appealSendable(text())) return;
    setSending(true);
    setFailure(null);
    try {
      const outcome = await auth.appealSuspension(text());
      if (outcome === "sent") {
        setText("");
        props.onSent();
        return;
      }
      if (outcome === "not-suspended") {
        // The suspension ended meanwhile: nothing to appeal; the screen goes
        // away with the next refresh.
        props.onClose();
        return;
      }
      setFailure(appealFailureKey(outcome));
    } finally {
      setSending(false);
    }
  }

  const count = (): string => {
    const nf = new Intl.NumberFormat(displayLocale());
    return `${nf.format(appealLength(text()))}/${nf.format(APPEAL_MAX_CHARS)}`;
  };

  return (
    <ModalLayer
      open={props.open}
      title={t("suspended.appeal.title")}
      onClose={() => {
        if (!sending()) props.onClose();
      }}
      class="lk-report-sheet"
    >
      <form
        class="lk-stack lk-report-form"
        onSubmit={(e) => {
          e.preventDefault();
          void send();
        }}
      >
        <label class="lk-report-comment">
          <textarea
            rows={6}
            maxLength={APPEAL_MAX_CHARS}
            placeholder={t("suspended.appeal.field")}
            aria-label={t("suspended.appeal.field")}
            value={text()}
            onInput={(e) => setText(e.currentTarget.value)}
          />
          <span class="lk-meta lk-appeal-count" aria-hidden="true">
            {count()}
          </span>
        </label>
        <p class="lk-meta">{t("suspended.appeal.once")}</p>
        <Show when={failure()}>
          {(key) => (
            <div class="form-error" role="alert">
              {t(key())}
            </div>
          )}
        </Show>
        <div class="lk-actions lk-report-actions">
          <button type="submit" class="primary" disabled={sending() || !appealSendable(text())}>
            {sending() ? t("common.sending") : t("suspended.appeal.send")}
          </button>
        </div>
      </form>
    </ModalLayer>
  );
}
