// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `docs/ux/attachments.md` section 9 — send where the person is now, once.
 * The browser's own prompt asks for permission on entering this screen; the
 * fix keeps updating while it is open. Nothing leaves the device before
 * `Send`, and Lakko asks no server where the person is.
 */
import { createSignal, onCleanup, onMount, Show, type JSX } from "solid-js";

import { t } from "../l10n";
import { displayLocale } from "../l10n/display-locale";
import { Icon } from "../ui/icons";
import { AttachScreen } from "./AttachScreen";
import {
  formatAccuracy,
  formatCoordinates,
  isPoorFix,
  LOCATION_NAME_MAX,
  LOCATION_NO_FIX_MS,
  toSharedLocation,
} from "./location-format";
import type { SharedLocation } from "./types";
import "./editors.css";

type Phase = "finding" | "fix" | "denied" | "not-found" | "unavailable";

export function LocationScreen(props: {
  readonly onCancel: () => void;
  readonly onSend: (loc: SharedLocation) => Promise<void> | void;
}): JSX.Element {
  const [phase, setPhase] = createSignal<Phase>("finding");
  const [fix, setFix] = createSignal<SharedLocation | null>(null);
  const [name, setName] = createSignal("");
  const [now, setNow] = createSignal(Date.now());
  const [busy, setBusy] = createSignal(false);
  let watchId: number | null = null;
  let noFixTimer: ReturnType<typeof setTimeout> | null = null;

  function stop(): void {
    if (watchId !== null && typeof navigator !== "undefined" && navigator.geolocation !== undefined) {
      navigator.geolocation.clearWatch(watchId);
    }
    watchId = null;
    if (noFixTimer !== null) clearTimeout(noFixTimer);
    noFixTimer = null;
  }

  function start(): void {
    stop();
    setFix(null);
    if (typeof navigator === "undefined" || navigator.geolocation === undefined) {
      setPhase("unavailable");
      return;
    }
    setPhase("finding");
    noFixTimer = setTimeout(() => {
      if (fix() === null) {
        stop();
        setPhase("not-found");
      }
    }, LOCATION_NO_FIX_MS);
    watchId = navigator.geolocation.watchPosition(
      (pos) => {
        setFix(toSharedLocation(pos.coords, pos.timestamp, ""));
        setPhase("fix");
      },
      (err) => {
        if (err.code === err.PERMISSION_DENIED) {
          stop();
          setPhase("denied");
        } else if (fix() === null && err.code === err.POSITION_UNAVAILABLE) {
          // Keep waiting until the no-fix timer decides; a later fix may come.
        }
      },
      { enableHighAccuracy: true, maximumAge: 0, timeout: LOCATION_NO_FIX_MS },
    );
  }

  onMount(() => {
    start();
    const tick = setInterval(() => setNow(Date.now()), 5000);
    onCleanup(() => clearInterval(tick));
  });
  onCleanup(stop);

  async function send(): Promise<void> {
    const f = fix();
    if (f === null || busy()) return;
    setBusy(true);
    try {
      const trimmed = name().trim();
      await props.onSend({ ...f, ...(trimmed.length === 0 ? {} : { name: trimmed }) });
    } finally {
      setBusy(false);
    }
  }

  const locale = (): string => displayLocale();

  return (
    <AttachScreen
      title={t("location.title")}
      onClose={() => props.onCancel()}
      bodyClass="lk-attach-location"
      footer={
        <Show
          when={phase() !== "not-found"}
          fallback={
            <button type="button" class="primary" onClick={() => start()}>
              {t("poll.try_again")}
            </button>
          }
        >
          <button type="button" class="primary" disabled={fix() === null || busy()} onClick={() => void send()}>
            {t("location.send")}
          </button>
        </Show>
      }
    >
      <div class="lk-attach-pin" aria-hidden="true">
        <Icon name="pin" size={36} />
      </div>
      <Show when={phase() === "finding"}>
        <p class="lk-attach-note" aria-live="polite">
          {t("location.finding")}
        </p>
      </Show>
      <Show when={phase() === "denied" || phase() === "unavailable"}>
        <p class="lk-attach-error" role="alert">
          {phase() === "denied" ? t("location.denied") : t("attach.menu.no_location")}
        </p>
        <Show when={phase() === "denied"}>
          <p class="lk-attach-note">{t("location.allow_browser")}</p>
        </Show>
      </Show>
      <Show when={phase() === "not-found"}>
        <p class="lk-attach-error" role="alert">
          {t("location.not_found")}
        </p>
      </Show>
      <Show when={fix()}>
        {(f) => (
          <>
            <p class="lk-attach-coords">{formatCoordinates(f().latE7, f().lonE7, locale())}</p>
            <p
              class="lk-attach-note"
              classList={{ "lk-attach-warn": isPoorFix(f().accuracyM, f().fixAtMs, now()) }}
              aria-live="polite"
            >
              {t("location.accuracy", { distance: formatAccuracy(f().accuracyM, locale()) })}
            </p>
          </>
        )}
      </Show>
      <Show when={phase() === "fix" || phase() === "finding"}>
        <input
          class="lk-attach-field"
          type="text"
          maxLength={LOCATION_NAME_MAX}
          placeholder={t("location.name_place")}
          aria-label={t("location.name_place")}
          value={name()}
          onInput={(e) => setName(e.currentTarget.value)}
        />
      </Show>
    </AttachScreen>
  );
}
