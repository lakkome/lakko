// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Links to the Terms of Service and the Privacy Policy (docs/ux/legal-pages.md
 * section 2): static pages on the deployment's web origin, opened in a new tab
 * on the web and in the system browser on desktop. Never an in-app view.
 */
import type { JSX } from "solid-js";

import { t } from "../l10n";
import { tRich } from "../l10n/rich";
import { isDesktop, publicOrigin } from "../platform";
import { openHostUrl } from "../platform/host";

export type LegalPage = "terms" | "privacy";

/** The page's address on the deployment's web origin. */
export function legalPageUrl(page: LegalPage): string {
  let origin: string;
  try {
    origin = publicOrigin();
  } catch {
    origin = "https://lakko.me";
  }
  return `${origin}/${page}`;
}

/** Open the page the way the platform opens links: on desktop the system
 *  browser, elsewhere the anchor's own new tab. */
function onOpen(page: LegalPage, e: MouseEvent): void {
  if (!isDesktop()) return;
  e.preventDefault();
  void openHostUrl(legalPageUrl(page));
}

/** An anchor around `children` that opens the page. */
export function LegalAnchor(props: {
  readonly page: LegalPage;
  readonly class?: string;
  readonly children: JSX.Element;
}): JSX.Element {
  return (
    <a
      class={props.class}
      href={legalPageUrl(props.page)}
      target="_blank"
      rel="noopener noreferrer"
      onClick={(e) => onOpen(props.page, e)}
    >
      {props.children}
    </a>
  );
}

/** The muted line under a Create account button. */
export function SignupLegalNotice(props: { readonly class?: string }): JSX.Element {
  return (
    <p class={props.class ?? "form-info"}>
      {tRich("legal.signup_notice", {
        terms: <LegalAnchor page="terms">{t("legal.terms")}</LegalAnchor>,
        privacy: <LegalAnchor page="privacy">{t("legal.privacy")}</LegalAnchor>,
      })}
    </p>
  );
}
