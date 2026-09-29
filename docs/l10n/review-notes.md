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

## Batch 2: settings

### Across languages

- **"Guest"** (`call.guest.default_name`, `settings.me.guest`) is grammatically
  masculine in es "Invitado", pt-BR "Convidado", he "אורח". It becomes a
  profile name, so a neutral word would be better if one exists.
- **"security key"** (hardware, `settings.passkeys.lede`): ru and uk use
  "аппаратный ключ" / "апаратний ключ", because "ключ безопасности" is
  already the ru term for the account identity key
  (`security.identity_changed.title`).
- **"video notes"** has no settled term: ru/uk видеосообщения, pt-BR
  "mensagens de vídeo", es "videonotas", tr "görüntülü notlar", ar/fa
  literal "notes", zh-Hans 视频消息. Must match the chat screen once it is
  extracted.
- **`common.remove`** equals `common.dismiss` in hi ("हटाएँ"). Acceptable
  only if no screen shows both.
- **Quotation marks around arguments** follow each language's existing
  style: «» (ru, uk, es, ar, fa), „” (he), “” (zh-Hans), ASCII (pt-BR, tr,
  hi). `settings.devices.add.no_camera` quotes `{method}` and `{prompt}`;
  check on screen that the quoted names match the recovery screen.
- **`settings.recovery.contact.sent_email` / `sent_phone`:** {contact}
  is rendered as an element; tr moves it after a colon to avoid a case
  suffix on the placeholder, as in batch 1.
- **`settings.account.vault.on`:** zh-Hans wraps {positions} in
  "第 … 个词", which reads well only if the list formatter uses 、.
- **`settings.passkeys.added_last_used`:** ru, uk, es, pt-BR, ar, hi agree
  the participle with "passkey"; the English has no subject, so check the
  gender is right in each.

### Per language

- **es:** "computadoras" (Latin American) in `settings.devices.lede`; Spain
  would say "ordenadores".
- **he:** `call.link.host` uses "מארח/ת" to stay neutral; Lakko stays
  feminine (מגנה) as in batch 1.
- **zh-Hans:** `settings.passkeys.lede` uses Apple's Chinese names 触控 ID
  and 面容 ID instead of the English Touch ID / Face ID.
- **tr:** `settings.passkeys.remove.confirm_named` reads "Kaldır: {name}"
  (label form) to avoid a case suffix; stiff for a button.

### Batch 2 top-up (keys added by iOS and Android)

- **"Identity"** section heading (`settings.recovery.section.identity`): ru
  keeps the batch-1 term "Ключи аккаунта" (also in `recovery.phrase.sub`);
  uk "Ідентичність"; the others use their plain word for identity. Settle
  this with the batch-1 "identity" doubt.
- **`settings.account.danger_zone.footer`** quotes the three row labels
  in ru, uk, ar, fa, he, hi, zh-Hans and leaves them bare in es, pt-BR,
  tr; the quoted words must match the row labels exactly.
- **`settings.devices.remove.*`:** `{device}` is a row label ("This
  device", "Device 2") inserted mid-sentence; ru/uk use "Удалить:
  {device}" and tr keeps "Kaldır: {device}" to avoid case endings;
  zh-Hans joins it with no space ("移除{device}").
- **Anti-censorship title** differs in register: ru/uk "Обход блокировок /
  блокувань", es/pt-BR "Anticensura", tr "Sansürü aşma", ar "تجاوز الرقابة",
  he "עקיפת צנזורה", fa "ضدسانسور", hi "सेंसरशिप से बचाव", zh-Hans "反审查".
  The word can be sensitive in some places; a neutral name ("Hidden
  connection") may be safer.
- **`settings.profile.name.hint`:** ru and uk use genitive after "не более /
  не більше" in every category; ar uses the six-form plural; zh-Hans has
  `other` only.
- **Lakko gender in he** stays feminine (מתחברת, יכולה) in the
  anti-censorship and notifications strings.
