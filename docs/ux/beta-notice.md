<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# UX spec: the beta notice

**Status:** accepted 2026-11-19 (maintainer). Built the same day on web and desktop, iOS and
Android; unit-tested and built, not looked at on a device or in a browser. iOS and Android
gained the About row "How Lakko protects you" with it (web had it; it follows Licence on
all three).
**Surfaces:** web and desktop (one code base), iOS, Android, and the store beta text.
**Why:** condition 3 of the [ADR-0046 amendment](../adr/0046-external-crypto-review.md):
while the cryptography has not been independently reviewed, a person sees that before
signing up, on the site, and in the store beta description.

## 1. Goal

Before a person creates an account, they learn in plain words that Lakko is a beta, that
an independent security audit is not finished yet, and that continuing means accepting
that risk (wording changed by the maintainer on 2026-11-20: the earlier "Do not rely on
it where your safety depends on it." drove people away). They are told once, clearly,
and are not asked to tick anything. People already using Lakko can find the same words in Settings.

## 2. The words

| Key | Text |
|---|---|
| `beta.notice.title` | Beta |
| `beta.notice.body` | Lakko is in beta. An independent security audit is not finished yet; we are working on it. We believe Lakko is safe to use, but we cannot prove it yet. By continuing, you accept this risk. |

Both are marked `security_critical`: until a person has reviewed a translation, every
language shows the English text. A mistranslated warning is worse than an English one.

## 3. Where it shows

1. **The first screen a signed-out person sees.** Web and desktop: the sign-in page
   (`/login`, where a signed-out visit to lakko.me lands). iOS and Android: the welcome
   screen.
2. **The sign-up screen**, above the form, and above the "Lakko is full for now" state
   of [`beta-account-cap.md`](beta-account-cap.md) when that state replaces the form.
3. **The guest's `Create account` form** after a call
   ([`call-links.md`](call-links.md) section 9): a guest who creates an account is
   signing up. It is not shown on the call-link page itself or in the call: joining a
   call as a guest creates nothing the person keeps, and the page has its own short
   guest line.
4. **Settings > About**, all clients, as a row like the others there: title
   `beta.notice.title`, text `beta.notice.body`. Placed first in About, above "How
   Lakko protects you", so the claim about protection is read with the caveat.

Places 1-3 use the design-system **Banner** (inline, sunken surface): the `info` icon,
the title in the banner's title style, the body under it. It has **no action and no
close button**: it informs and asks nothing (design-system Banner rule, extended in this
change). It does not move or change with state; it is not a dialog, toast or overlay.

## 4. States

There are none. The banner is static text with no loading, error or dismissed state, and
it never depends on the network. It is shown in every build until the beta ends; when the
external review is done the strings and this spec are removed together (ADR-0046).

## 5. Accessibility

- The banner is one accessibility element read as "Beta. Lakko is in beta, ..." (title
  then body); the icon is decorative.
- It is not focusable (nothing to do in it) and sits before the form in reading and focus
  order, so a screen reader reaches it before the first field.
- Text follows dynamic type / font scaling and wraps; no truncation.
- Colour carries no meaning: the sunken surface and the icon are the same as other
  informational banners, not a warning colour.

## 6. Store beta text

TestFlight "Beta App Description" and the Google Play testing-track description open with
the same two sentences (English; translations reviewed before use), then what the beta
contains. The draft is in [`docs/beta-store-text.md`](../beta-store-text.md).

## 7. What the data has to provide

Nothing. No server, wire or core change.
