<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# UX spec: Terms of Service and Privacy Policy

**Status:** accepted 2026-11-19 (maintainer). Built the same day on web and desktop, iOS and
Android, unit-tested, not walked. The web build renders the pages
(`apps/web/scripts/legal-pages.mjs`); the service worker never caches them. Page texts are drafts with
TODOs; until they are filled, the pages show "This page is being prepared."
**Surfaces:** the two pages on lakko.me; web and desktop, iOS, Android.

## 1. The pages

- `https://lakko.me/terms` (Terms of Service) and `https://lakko.me/privacy` (Privacy
  Policy): static HTML in the web build, served by nginx as files, readable without
  signing in, without JavaScript and without the app. Sources:
  [`docs/legal/terms.md`](../legal/terms.md), [`docs/legal/privacy.md`](../legal/privacy.md).
- English only during the beta; the pages say so in one line. Each page shows its
  "Last updated" date.
- Light and dark follow the system; phone width works without horizontal scroll.

## 2. Where the app links to them

1. **Sign-up screen**, under the Create account button, one muted line:

   > By creating an account, you agree to the {terms} and the {privacy}.

   `{terms}` reads "Terms of Service", `{privacy}` reads "Privacy Policy"; each is a link.
   No checkbox: creating the account is the act (the policy's legal basis is the
   contract, not consent).
2. **The guest's `Create account` form** after a call: the same line under its button.
   Joining a call as a guest shows nothing new.
3. **Settings > About**, two rows after "How Lakko protects you": `legal.terms_title` and
   `legal.privacy_title`, each opening its page.

A link opens the page in the system browser on iOS, Android and desktop, and in a new
tab on the web (`docs/ux/message-links.md` opens links the same way). No in-app web view.

## 3. Strings (`l10n/catalog/en.json`)

| Key | Text |
|---|---|
| `legal.signup_notice` | By creating an account, you agree to the {terms} and the {privacy}. |
| `legal.terms` | Terms of Service |
| `legal.privacy` | Privacy Policy |
| `legal.terms_title` | Terms of Service (the Settings row; a separate key because other languages inflect the link text inside the sentence) |
| `legal.privacy_title` | Privacy Policy (the Settings row) |

They are translated like the rest of the interface (not security-critical); the pages
themselves stay English.

## 4. Data

Nothing new. The URLs are fixed paths on the deployment's web origin.
