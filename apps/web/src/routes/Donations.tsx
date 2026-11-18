// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `/donations`: the ways to donate to Lakko (`docs/ux/donations.md`).
 * Public: signed in or not, the same page. Opened from Settings > About on
 * web and in the desktop app. Laid out at the app's page width (section
 * 2.0): two groups, Bank and card then Crypto, each a grid of equal-height
 * cards (one column on a phone, two from 640 px, three from 960 px).
 *
 * Every method, QR image and phone number shown here verified against the
 * donations key pinned into this build (`src/donate/client.ts`); a build with
 * no pinned key, or an answer where nothing verifies, shows `donate.none`.
 */
import { createMemo, createResource, createSignal, For, Match, onCleanup, Show, Switch, type JSX } from "solid-js";
import { useNavigate } from "@solidjs/router";

import { PublicWideLayout } from "../ui/PublicLayout";
import { t } from "../l10n";
import { isDesktop } from "../platform";
import { openHostUrl } from "../platform/host";
import { paths } from "./paths";
import {
  fetchDonateMethods,
  fetchDonateQr,
  groupDonateMethods,
  MASKED_PHONE,
  revealDonatePhone,
  type DonateIcon,
  type DonateMethod,
} from "../donate/client";
import { donateQrSvg, planDonateQr } from "../donate/donate-qr";

/** The side, in CSS pixels, of every QR on the page (section 2.4). */
const QR_SIDE = 200;

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

function CopyButton(props: {
  readonly text: string;
  readonly label: string;
  readonly target: () => HTMLElement | undefined;
  readonly class?: string;
}): JSX.Element {
  const [copied, setCopied] = createSignal(false);
  return (
    <button
      type="button"
      class={props.class ?? "small"}
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
            <button type="button" class="small" disabled={busy()} onClick={() => void reveal()}>
              {t("donate.phone_show")}
            </button>
          }
        >
          {(p) => <CopyButton text={p()} label={t("donate.copy")} target={() => field} class="small" />}
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
 *  SHA-256; on a mismatch or a failure nothing is shown (not even the space
 *  kept for it while it loads) and the rest of the card stays. */
function VerifiedQr(props: { readonly id: string; readonly sha256: Uint8Array }): JSX.Element {
  const [url, setUrl] = createSignal<string | null | undefined>(undefined);
  let alive = true;
  onCleanup(() => {
    alive = false;
    const u = url();
    if (typeof u === "string") URL.revokeObjectURL(u);
  });
  void fetchDonateQr(props.id, props.sha256).then((blob) => {
    if (!alive) return;
    setUrl(blob === null ? null : URL.createObjectURL(blob));
  });
  return (
    <Show when={url() !== null}>
      <div class="lk-donate-qr-slot">
        <Show when={url()}>
          {(u) => <img class="lk-donate-qr" src={u()} alt={t("common.qr_code")} width={QR_SIDE} height={QR_SIDE} />}
        </Show>
      </div>
    </Show>
  );
}

function PaymentCard(props: { readonly method: DonateMethod }): JSX.Element {
  const m = (): DonateMethod => props.method;
  const name = (): string => (m().kind === "crypto" ? "" : METHOD_NAMES[m().kind as Exclude<DonateMethod["kind"], "crypto">]);
  return (
    <article class="lk-card lk-donate-card">
      <div class="lk-donate-head">
        <h3>{name()}</h3>
        <p class="lk-donate-sub">{m().kind === "sbp" ? t("donate.for_russia") : t("donate.worldwide")}</p>
      </div>
      <Show when={m().qrSha256}>
        {(sha) => <VerifiedQr id={m().id} sha256={sha()} />}
      </Show>
      <div class="lk-donate-actions">
        <Show when={m().link}>
          {(link) => (
            <button type="button" class="primary lk-donate-wide" onClick={() => openPayment(link())}>
              {t("donate.button")}
            </button>
          )}
        </Show>
        <Show when={m().hasPhone}>
          <PhoneRow id={m().id} />
        </Show>
      </div>
    </article>
  );
}

/** A wallet address's QR, drawn here from the verified address with the
 *  signed coin mark in the centre (`src/donate/donate-qr.ts`); never the
 *  branded QR, which carries the Lakko logo. */
function AddressQr(props: { readonly address: string; readonly icon: DonateIcon }): JSX.Element {
  // The markup is built only from the encoder's module matrix, fixed colours
  // and the bundled coin artwork; the address decides which modules are dark
  // and never appears as text in it.
  const svg = (): string => donateQrSvg(planDonateQr(props.address, props.icon), t("common.qr_code"));
  return <div class="lk-donate-qr lk-donate-qr-drawn" innerHTML={svg()} />;
}

function CryptoCard(props: {
  readonly coin: string;
  readonly network: string;
  readonly address: string;
  readonly icon: DonateIcon;
}): JSX.Element {
  let field: HTMLSpanElement | undefined;
  return (
    <article class="lk-card lk-donate-card">
      <div class="lk-donate-head">
        <h3>{props.coin}</h3>
        <p class="lk-donate-sub">{t("donate.crypto_network", { network: props.network })}</p>
      </div>
      <div class="lk-donate-qr-slot">
        <AddressQr address={props.address} icon={props.icon} />
      </div>
      <div class="lk-donate-actions">
        <span class="lk-donate-mono lk-donate-address" ref={field}>
          {props.address}
        </span>
        <p class="lk-donate-warning">
          {t("donate.crypto_warning", { coin: props.coin, network: props.network })}
        </p>
        <CopyButton
          text={props.address}
          label={t("donate.crypto_copy")}
          target={() => field}
          class="primary lk-donate-wide"
        />
      </div>
    </article>
  );
}

function MethodCard(props: { readonly method: DonateMethod }): JSX.Element {
  return (
    <Show when={props.method.crypto} fallback={<PaymentCard method={props.method} />}>
      {(c) => <CryptoCard coin={c().coin} network={c().network} address={c().address} icon={c().icon} />}
    </Show>
  );
}

/** One group: its heading, then its cards in a grid. Not drawn when empty. */
function Group(props: { readonly title: string; readonly methods: readonly DonateMethod[] }): JSX.Element {
  return (
    <Show when={props.methods.length > 0}>
      <section class="lk-donate-group">
        <h2 class="lk-donate-group-title">{props.title}</h2>
        <div class="lk-donate-grid">
          <For each={props.methods}>{(m) => <MethodCard method={m} />}</For>
        </div>
      </section>
    </Show>
  );
}

export default function Donations(): JSX.Element {
  const navigate = useNavigate();
  const [methods, { refetch }] = createResource(() => fetchDonateMethods());
  const groups = createMemo(() => groupDonateMethods(methods() ?? []));
  const back = (): void => {
    if (window.history.length > 1) window.history.back();
    else navigate(paths.home);
  };
  return (
    <PublicWideLayout>
      <div class="lk-donate">
        <button type="button" class="ghost small lk-donate-back" onClick={back}>
          {t("common.back")}
        </button>
        <header class="lk-donate-intro">
          <h1>{t("donate.title")}</h1>
          <p class="lk-donate-lede">{t("donate.lede")}</p>
        </header>
        <Switch>
          <Match when={methods.loading}>
            <div class="lk-donate-grid" aria-busy="true">
              <div class="lk-donate-placeholder" />
              <div class="lk-donate-placeholder" />
            </div>
          </Match>
          <Match when={methods.error !== undefined}>
            <p class="lk-donate-error" role="alert">
              {t("donate.error")}
            </p>
            <button type="button" class="primary lk-donate-retry" onClick={() => void refetch()}>
              {t("common.try_again")}
            </button>
          </Match>
          <Match when={(methods() ?? []).length === 0}>
            <p class="lk-donate-none">{t("donate.none")}</p>
          </Match>
          <Match when={true}>
            <Group title={t("donate.section_payment")} methods={groups().payment} />
            <Group title={t("donate.section_crypto")} methods={groups().crypto} />
          </Match>
        </Switch>
      </div>
    </PublicWideLayout>
  );
}
