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

## Batch 3 (chat, chat list, common): ar, es, fa, he, hi

### Across languages

- **"video message" = the round video note.** The source says "Video message"
  in chat keys; each language reuses its existing video-note term from
  `settings.media.autoplay_video_notes.*` for consistency: es "videonota",
  ar "ملاحظة فيديو", fa "یادداشت ویدیویی", he "הודעת וידאו", hi "वीडियो नोट".
- **`chat.record.slide_cancel`**: ar, fa, he flipped the arrow to "›" on the
  assumption the cancel gesture is mirrored in RTL; es and hi keep "‹". Check
  on screen which way the slide actually goes.
- **`chat.members.add.placeholder`**: ar, fa, he, hi keep "@username" in Latin
  (usernames are `[a-z0-9_]`); es uses "@usuario".
- **`chat.gif.attribution`**: "Powered by" translated, GIPHY kept. If GIPHY's
  terms require the English line or their logo, revert.
- **"applies when an owner or an admin applies the change"**: rendered as
  "takes effect when the owner or an admin applies the change" (es "el
  propietario"); the group has one owner.
- **File-size units**: es kept B/kB/MB/GB; ar and fa spell the units out;
  he "בתים" + KB/MB/GB; hi "बाइट" + KB/MB/GB (kB became KB in he and hi).
- **`common.duration.minutes_seconds`** gained "and" in ar ("و"), fa ("و"),
  he ("ו-"); es and hi keep a plain space.

### Per language

- **ar**: `chat.forward.label` "رسالة معاد توجيهها" (a noun phrase); "tap"
  unified to "اضغط" as in settings. `chat.message.react` is the imperative
  "تفاعل" and `chat.reaction.*` "تفاعل {names}" reads as a past verb: same
  spelling, check context.
- **es**: `chat.members.admin.*` and "administrador"/"propietario" are
  masculine; `chat.author.you` "Tú". `chat.peer.fallback_name` "Contacto"
  rather than a literal "Par".
- **fa**: forward = "هدایت" (Telegram usage); pin = "سنجاق"; conversation and
  chat both "گفتگو", so `chat.delete.control` says "this conversation".
- **he**: "conversation" is "צ׳אט" throughout, because "שיחה" is also "call".
  Gendered leftovers: "חבר" (member), "מנהל" (admin), "שולח", "ממתין";
  `chat.author.you` and `chats.preview.own` use "את/ה" (the slash form flagged
  in batch 1). Typing uses the neutral "בהקלדה". Owner = "בעלים" (plural
  agreement). Arguments after prepositions use a hyphen: "מ-{name}",
  "ל-{name}".
- **hi**: "owner" = "मालिक", ownership = "मालिकाना हक"; group = "ग्रुप"
  (not समूह); `chat.pinned_bar.label_of` reworded to "{index}, कुल {count}".

## Batch 3 (chat, chat list, common): pt-BR, ru, tr, uk, zh-Hans

- **Owner / admin terms.** pt-BR "proprietário" / "administrador" and
  tr "sahip" / "yönetici" are grammatically masculine or generic; ru/uk
  "владелец / власник", "администратор / адміністратор" are masculine
  generics. zh-Hans uses 群主 for the group owner (distinct from 发起人,
  the call-link owner from batch 1).
- **Placeholder `chat.members.add.placeholder`:** ru, uk keep `@username`
  in Latin (usernames are `[a-z0-9_]`, a Cyrillic word would suggest
  otherwise); pt-BR `@nomedeusuario`, tr `@kullaniciadi` (ASCII);
  zh-Hans `@用户名` (inconsistent with ru/uk; pick one rule).
- **Video note term** follows batch 2: tr keeps "görüntülü not" for
  "Video message" (`chat.media.video_message`, `chat.record.*`), which
  reads oddly next to "Sesli mesaj"; pt-BR "mensagem de vídeo", ru/uk
  видео-/відеоповідомлення, zh-Hans 视频消息.
- **Label form to avoid case endings on `{name}`:** ru/uk
  "Удалить: {name}", "Выйти: {name}", "Добавлено: {name}", "Переслано
  от: {name}", "Ответ: {name}" (both `quote_to` and `strip_label`
  render the same in ru/uk); tr uses label form throughout
  ("Sil: {name}", "Ara: {name}", "Bulunamadı: {name}",
  "Şu kişiden iletildi: {name}", "davet eden: {name}").
  zh-Hans joins `{name}` with no space ("删除{name}", "退出{name}").
- **Plurals with an identical form in every category** (ru/uk
  `chat.select.count`, `chat.media.and_more` put the number outside the
  noun, "Выбрано: #"); tr repeats the singular noun in both
  categories, as Turkish does after a number.
- **`chat.members.add.done` (pt-BR)** rephrased to "{name} agora está no
  grupo." and admin notices to "Função de administrador
  removida/concedida" to avoid gender; ru "Назначен администратор." is
  still masculine.
- **`chat.transfer.new_owner_named`:** ru/uk use the present
  "передаёт / передає" to avoid a gendered past tense.
- **`chat.gif.attribution`:** translated ("Com tecnologia GIPHY", "При
  поддержке GIPHY", "GIPHY tarafından desteklenmektedir", "На основі
  GIPHY", "由 GIPHY 提供支持"); GIPHY's own attribution rules may require
  the English "Powered by GIPHY" logo text.
- **`chat.record.pause`** ("Pause recording", comment: pauses playback of
  the recording): zh-Hans says "暂停播放录音"; the others translate the
  English literally and are equally ambiguous.
- **`common.duration.minutes_seconds`:** pt-BR inserts "e" ("{minutes} e
  {seconds}"); zh-Hans joins with no space; ru/uk/tr keep a space.
- **Sizes:** ru/uk use Cyrillic units (Б, КБ, МБ, ГБ); zh-Hans "KB";
  pt-BR/tr keep "kB" as the source.
- **`chat.peer.fallback_name`** ("Peer"): pt-BR "Contato", ru
  "Собеседник", uk "Співрозмовник", tr "Karşı taraf", zh-Hans "对方".
- **`chats.row.call.label`** ("{kind} from {name}"): ru/uk/tr "{kind}:
  {name}"; zh-Hans reorders to "来自 {name} 的{kind}".

### Batch 3 top-up (keys added by iOS and Android)


- **`chat.composer.photo_library`**: es "Fototeca" follows Apple's Spanish
  name; on Android "Galería" may read better. Others are literal.
- **`chat.record.open_settings`**: es "Abrir Ajustes" (iOS term); Android
  calls it "Configuración".
- **`common.ok`**: es "Aceptar", ar "حسنًا", fa "باشه" (informal; "تأیید" is
  more formal), he "אישור", hi "ठीक है". hi `chat.gif.note_dismiss` is also
  "ठीक है".
- **he**: `chat.record.locked_hint` "מתחיל" and `chat.media.open_full_hint`
  "פותח" are masculine, agreeing with the button. `chat.members.count` uses
  "חברים" (the masculine plural used generically).
- **`chat.video_note.poster`**: rendered as "cover/preview image" in each
  language; the literal "poster" means nothing to a user.


- **`chat.composer.photo_library`:** follows the platform names: pt-BR
  "Galeria de fotos", ru "Медиатека", uk "Медіатека" (the Apple Photos
  picker name), tr "Fotoğraf Arşivi", zh-Hans 照片图库. On Android the
  system name differs; check both platforms.
- **`chat.record.open_settings`:** pt-BR "Abrir Ajustes" uses the iOS name
  "Ajustes"; Android says "Configurações", and earlier batches used
  "Configurações". Pick per platform or neutral.
- **`chat.record.locked_hint`:** "locked recording" rendered as "hands-free"
  (ru/uk "без удержания", tr "basılı tutmadan", zh-Hans 免按住,
  pt-BR "contínua").
- **`chat.photo.viewer_hint`:** ru/uk spell out the pinch gesture
  ("Сведите или разведите пальцы"); long for a hint.
- **`common.ok`:** ru "ОК" is Cyrillic, uk keeps Latin "OK"; pick one.
- **`chat.reaction.reacted_by_title`** is a heading with no object:
  pt-BR "Reações de" dangles; ru/uk "Отреагировали", tr "Tepki verenler",
  zh-Hans 回应者 read as "Those who reacted".
- **`chats.new_group.accept_again`:** pt-BR "adicioná-lo", ru/uk "его/його"
  refer to "contact" (grammatically masculine), not the person.
- **tr "Video message"** stays "görüntülü not" in the new keys, as in the
  main batch.
