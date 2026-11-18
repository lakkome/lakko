// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `/donations`: the ways to donate to Lakko (`docs/ux/donations.md`).
 * Public: signed in or not, the same page. Opened from Settings > About on
 * web and in the desktop app; iOS and Android open this address in the
 * system browser.
 *
 * Every method, QR image and phone number shown here verified against the
 * donations key pinned into this build (`src/donate/client.ts`); a build with
 * no pinned key, or an answer where nothing verifies, shows `donate.none`.
 */
import { createResource, createSignal, For, Match, onCleanup, Show, Switch, type JSX } from "solid-js";
import { useNavigate } from "@solidjs/router";

import { PublicLayout } from "../ui/PublicLayout";
import { BrandedQr } from "../ui/BrandedQr";
import { t } from "../l10n";
import { isDesktop } from "../platform";
import { openHostUrl } from "../platform/host";
import { paths } from "./paths";
import {
  fetchDonateMethods,
  fetchDonateQr,
  MASKED_PHONE,
  revealDonatePhone,
  type DonateMethod,
} from "../donate/client";

const METHOD_NAMES: Record<Exclude<DonateMethod["kind"], "crypto">, string> = {
  sbp: "SBP",
  revolut: "Revolut",
  bmc: "Buy Me a Coffee",
};

/** Put `text` on the clipboard; on failure, select `fallback` so the person
 *  can copy it by hand. */
async function copyOrSelect(text: string, fallback: HTMLElement | undefined): Promise<boolean> {
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch {
    if (fallback !== undefined) {
      const range = document.createRange();
      range.selectNodeContents(fallback);
      const sel = window.getSelection();
      sel?.removeAllRanges();
      sel?.addRange(range);
    }
    return false;
  }
}

function openPayment(url: string): void {
  if (isDesktop()) {
    void openHostUrl(url);
  } else {
    window.open(url, "_blank", "noopener,noreferrer");
  }
}

function CopyButton(props: { readonly text: string; readonly label: string; readonly target: () => HTMLElement | undefined }): JSX.Element {
  const [copied, setCopied] = createSignal(false);
  return (
    <button
      type="button"
      class="ghost small"
      onClick={() => {
        void copyOrSelect(props.text, props.target()).then((ok) => {
          if (!ok) return;
          setCopied(true);
          setTimeout(() => setCopied(false), 2000);
        });
      }}
    >
      {copied() ? t("donate.copied") : props.label}
    </button>
  );
}

function PhoneRow(props: { readonly id: string }): JSX.Element {
  const [phone, setPhone] = createSignal<string | null>(null);
  const [busy, setBusy] = createSignal(false);
  const [failed, setFailed] = createSignal(false);
  let field: HTMLSpanElement | undefined;
  const reveal = async (): Promise<void> => {
    setBusy(true);
    setFailed(false);
    try {
      setPhone(await revealDonatePhone(props.id));
    } catch {
      setFailed(true);
    } finally {
      setBusy(false);
    }
  };
  return (
    <div class="lk-donate-phone">
      <div class="lk-donate-phone-row">
        <span class="lk-donate-mono" ref={field} aria-label={phone() === null ? t("donate.phone_hidden") : undefined}>
          {phone() ?? MASKED_PHONE}
        </span>
        <Show
          when={phone()}
          fallback={
            <button type="button" class="ghost small" disabled={busy()} onClick={() => void reveal()}>
              {t("donate.phone_show")}
            </button>
          }
        >
          {(p) => <CopyButton text={p()} label={t("donate.copy")} target={() => field} />}
        </Show>
      </div>
      <Show when={failed()}>
        <p class="lk-donate-error" role="alert">
          {t("donate.phone_error")}
        </p>
      </Show>
    </div>
  );
}

/** A method's QR image, shown only once its bytes hash to the signed
 *  SHA-256; on a mismatch or a failure nothing is shown and the rest of the
 *  card stays. */
function VerifiedQr(props: { readonly id: string; readonly sha256: Uint8Array }): JSX.Element {
  const [url, setUrl] = createSignal<string | null>(null);
  let alive = true;
  onCleanup(() => {
    alive = false;
    const u = url();
    if (u !== null) URL.revokeObjectURL(u);
  });
  void fetchDonateQr(props.id, props.sha256).then((blob) => {
    if (!alive || blob === null) return;
    setUrl(URL.createObjectURL(blob));
  });
  return (
    <Show when={url()}>
      {(u) => <img class="lk-donate-qr" src={u()} alt={t("common.qr_code")} width="200" height="200" />}
    </Show>
  );
}

function PaymentCard(props: { readonly method: DonateMethod }): JSX.Element {
  const m = (): DonateMethod => props.method;
  const name = (): string => (m().kind === "crypto" ? "" : METHOD_NAMES[m().kind as Exclude<DonateMethod["kind"], "crypto">]);
  return (
    <section class="lk-card lk-donate-card">
      <h2>{name()}</h2>
      <p class="lk-donate-sub">{m().kind === "sbp" ? t("donate.for_russia") : t("donate.worldwide")}</p>
      <Show when={m().qrSha256}>
        {(sha) => <VerifiedQr id={m().id} sha256={sha()} />}
      </Show>
      <Show when={m().link}>
        {(link) => (
          <button type="button" class="primary" onClick={() => openPayment(link())}>
            {t("donate.button")}
          </button>
        )}
      </Show>
      <Show when={m().hasPhone}>
        <PhoneRow id={m().id} />
      </Show>
    </section>
  );
}

function CryptoCard(props: { readonly coin: string; readonly network: string; readonly address: string }): JSX.Element {
  let field: HTMLSpanElement | undefined;
  return (
    <section class="lk-card lk-donate-card">
      <h2>{props.coin}</h2>
      <p class="lk-donate-sub">{t("donate.crypto_network", { network: props.network })}</p>
      <BrandedQr text={props.address} size={200} />
      <span class="lk-donate-mono lk-donate-address" ref={field}>
        {props.address}
      </span>
      <CopyButton text={props.address} label={t("donate.crypto_copy")} target={() => field} />
      <p class="lk-donate-warning">
        {t("donate.crypto_warning", { coin: props.coin, network: props.network })}
      </p>
    </section>
  );
}

export default function Donations(): JSX.Element {
  const navigate = useNavigate();
  const [methods, { refetch }] = createResource(() => fetchDonateMethods());
  const back = (): void => {
    if (window.history.length > 1) window.history.back();
    else navigate(paths.home);
  };
  return (
    <PublicLayout>
      <button type="button" class="ghost small lk-donate-back" onClick={back}>
        {t("common.back")}
      </button>
      <h1>{t("donate.title")}</h1>
      <p class="lk-donate-lede">{t("donate.lede")}</p>
      <Switch>
        <Match when={methods.loading}>
          <div class="lk-donate-placeholder" aria-busy="true" />
          <div class="lk-donate-placeholder" aria-hidden="true" />
        </Match>
        <Match when={methods.error !== undefined}>
          <p class="lk-donate-error" role="alert">
            {t("donate.error")}
          </p>
          <button type="button" class="primary" onClick={() => void refetch()}>
            {t("common.try_again")}
          </button>
        </Match>
        <Match when={(methods() ?? []).length === 0}>
          <p>{t("donate.none")}</p>
        </Match>
        <Match when={true}>
          <For each={methods()}>
            {(m) => (
              <Show when={m.crypto} fallback={<PaymentCard method={m} />}>
                {(c) => <CryptoCard coin={c().coin} network={c().network} address={c().address} />}
              </Show>
            )}
          </For>
        </Match>
      </Switch>
    </PublicLayout>
  );
}
