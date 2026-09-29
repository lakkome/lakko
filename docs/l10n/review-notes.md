<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# Translation review notes (ADR-0058 S5)

Every translation in `l10n/catalog/<lang>.json` is a machine translation
(`status: "machine"`) until a native speaker reviews it. Keys marked
`security_critical` in `en.json` ship in English until their translation is
`reviewed`. This file collects what the translation passes flagged, so a
reviewer starts from the known doubts. Batch 1 is the sign-in, sign-up and
recovery area (`docs/l10n/batch-1-auth.md`).

## Batch 1: sign-in, sign-up, recovery

### Across languages

- **Gendered first person and welcome lines.** es `auth.login.no_account`
  ("¿Eres nuevo…?"), pt-BR "Bem-vindo de volta" / "Novo no Lakko?",
  ru and uk "записал(а)" / "записав(-ла)"
  (`recovery_phrase.fresh.acknowledge`, `recovery_phrase.save.written`),
  he "מבין/ה". Prefer a neutral wording where the language has one.
- **`common.dismiss`** came out identical to `common.close` in ar, fa, he,
  tr, uk, zh-Hans. Acceptable only if no screen shows both.
- **"identity".** ru renders it as "ключи аккаунта" in `restore_identity.*`
  and `device_link.error.*` (mostly security-critical) and uses
  "Ключ безопасности" for `security.identity_changed.title`; uk uses
  "ідентичність" and "ключ безпеки". Pick one term per language and keep
  it distinct from the safety number.
- **"directory"** in `restore_identity.error.unreachable` was translated
  literally in ar, fa, he, uk; it may mean nothing to a user.
- **Text attached to arguments.** he and tr attach a preposition or suffix to
  `#` / `{count}` (for example `ב{count…}`, `{# paya}`); ar uses
  `لـ {username}`; `{method}` sits in quotation marks in
  `recovery.shares.usage` (reads correctly only if the method name is itself
  translated). Check these on screen.
- **Digits.** fa spells out "24", "3 of 5" and "six-digit"; the other
  languages keep the literal digits of the English source. Word positions
  and codes are arguments and are always Latin digits (ADR-0058 open
  question 2).
- **Share term** differs: es/pt-BR "partes", tr "pay", he "חלק",
  zh-Hans "分片", uk "частка".
- **"Home"** (pt-BR "Início", zh-Hans "主页") must match the Home screen's
  own label once that screen is extracted.

### Per language

- **es:** "Reintentar" on the button, "Inténtalo de nuevo" in messages;
  `recovery_phrase.save.no_screenshot` renders "plain text file" as
  "sin cifrar"; `restore_identity.lede` uses the perfect tense where other
  messages use the simple past.
- **tr:** `device_link.error.identity_mismatch` (security-critical) splits
  the em dash into a second sentence.
- **uk:** `recovery.shares.panel_body` (security-critical) paraphrases
  "fewer reveal nothing"; `common.looking_up` is the generic "Пошук...";
  `auth.login.passkey` is longer than the English.
- **zh-Hans:** 手机 rather than 电话 for "phone"; `{method}` in quotation
  marks in `recovery.shares.usage`; the em dash in
  `device_link.error.identity_mismatch` became a comma. Glossary used:
  通行密钥, 恢复短语, 设备, 账号, 用户名, 登录, 注册, 安全码, 端到端加密.

### Batch 1 top-up (keys added by iOS and Android)

- **tr:** `recovery.cached_account`, `recovery.device.restoring`,
  `recovery.lede_account`, `username.hint.shown_as` are phrased as
  "label: {username}" to avoid a case suffix on the placeholder; stiff.
- **ar, he:** `auth.signup.username.hint` embeds "_" in right-to-left text;
  check rendering.
- **he:** "Lakko" is treated as feminine (בודקת), as in the first pass.
- **"link a device"** per language: ru привязать, uk прив’язати, es/pt-BR
  vincular, tr bağla, ar ربط, he קישור, fa اتصال, hi लिंक, zh-Hans 关联.
