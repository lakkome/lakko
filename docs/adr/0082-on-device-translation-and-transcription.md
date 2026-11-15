<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# ADR-0082: Translation and transcription happen on the device that shows the message, or not at all

- **Status:** Accepted (2026-10-25, maintainer). The maintainer chose the
  direction the same day (on the device only, no cloud path) and accepted the
  v1 decisions: results and per-conversation choices are not synced between
  the account's devices, there is no sender-attached transcript, and language
  packs come from the platform. **Amended 2026-10-27 (maintainer):** Android
  translates through the system `TranslationManager` where the system has an
  on-device translation service, and through ML Kit Translate only where it
  has none, accepting that ML Kit reports the chosen languages to Google
  (section 1,
  "As built"). **Amended 2026-11-15:** text language detection comes from the
  Rust core where the platform has none Lakko can use (Android always; the web
  when the browser's `LanguageDetector` is absent or its model is not on the
  device); iOS keeps `NLLanguageRecognizer` (note of 2026-11-15).
- **Date:** 2026-10-25
- **Context:** media messages (ADR-0047), message actions (ADR-0052), interface
  localization (ADR-0061), store gate (ADR-0077), anti-censorship transport
  (ADR-0026). UX spec:
  [`docs/ux/translation-and-transcription.md`](../ux/translation-and-transcription.md).

## Context

Translating a message and turning a voice message into text are features
people expect from a messenger. The common implementation sends the text or
the audio to a server. In Lakko a server that receives either receives the
plaintext of a message, which security invariant #2 forbids, and no consent
screen changes that: the person asking for a translation is not the person who
wrote the message, so it is not theirs to disclose.

Every platform Lakko ships on now has an engine that runs on the device. Each
has a caveat that decides the design: the default speech recognizer on Android
and in Chrome sends audio to a server unless told otherwise, and language
packs are downloaded from the platform vendor.

Facts below were checked on 2026-10-25 against vendor documentation, except
where marked; versions are what the documentation states, not what was run.

## Decision

**Translation and transcription run only on the device that shows the message,
on the decrypted text or audio, through an engine that is required to process
locally. When no such engine is available for a language, the feature is not
offered. There is no remote path, no fallback and no setting that enables one.
Nothing in the wire format changes.**

### 1. Engines per platform

| | Transcription | Translation | Language detection |
|---|---|---|---|
| iOS / iPadOS / macOS | `SpeechAnalyzer` + `SpeechTranscriber` (iOS 26+; models managed through `AssetInventory`). Below iOS 26: `SFSpeechRecognizer` with `requiresOnDeviceRecognition = true`, never without it | `TranslationSession` (iOS 18+), which Apple documents as processing on the device | `NLLanguageRecognizer` (NaturalLanguage framework; not re-checked for this ADR) |
| Android | `SpeechRecognizer.createOnDeviceSpeechRecognizer` (API 31+), only after `isOnDeviceRecognitionAvailable` returns true. `createSpeechRecognizer` is never used: its documentation says the implementation is likely to stream audio to remote servers | The system `android.view.translation.TranslationManager` (API 31+), only `createOnDeviceTranslator` and `getOnDeviceTranslationCapabilities`, never the deprecated `createTranslator`; on a device whose system reports no on-device translation capability (or below API 31), ML Kit Translate, whose data disclosure says it reports the configured source and target languages to Google (amended 2026-10-27; the choice is per device, not per language pair; never while anti-censorship is on) | No platform detector: `TextClassifier` may run in an OEM process and ML Kit Language Identification reports the identified languages. Since 2026-11-15 the core detector `detectTextLanguage` (note of that date) |
| Web | `SpeechRecognition` only with `processLocally = true`, which the draft specification defines as "must be done locally"; with it absent or unsupported, the feature is absent. Experimental; which browsers honour it is not established | `Translator` API (Chrome 138+, desktop only per Chrome's documentation; Firefox and Safari not supported) | `LanguageDetector` API (same availability), only with its model already on the device; otherwise, since 2026-11-15, the core detector `detectTextLanguage` (note of that date) |
| watchOS | none in v1 | none in v1 | none |

A platform capability probe on each client answers, per language, `ready`,
`needs-pack` or `unavailable`; the UI offers controls from that answer alone
(UX section 7). An engine whose local-only guarantee cannot be confirmed by
its own API is treated as `unavailable`.

### 2. Fail closed

Each engine call site asserts the local-only option it depends on
(`requiresOnDeviceRecognition`, the on-device factory, `processLocally`). A
call that would run without it is a bug, not a degraded mode. The clients carry
a test for each assertion (web: a source test in the shape of
`ui/no-system-share.test.ts` that refuses `new SpeechRecognition` without
`processLocally = true`; Android: a lint-style test that refuses
`createSpeechRecognizer`; iOS: a unit test that the request is built with the
on-device flag). This is the invariant #4 corollary for these features: no
silent downgrade to a remote engine.

### 3. Who transcribes: the recipient

The device that shows a voice or video message transcribes it. No transcript
travels in the envelope, so `AttachmentRef` and the message body are
unchanged. A sender-attached transcript is UX open question 2 and would be a
separate decision (a sender-asserted text about their own audio needs its own
label and bounds).

### 4. Language packs

Packs are downloaded by the platform (Apple, Google, the browser vendor) when
the person agrees to the dialog of UX 3.5. That download reveals to the vendor
which language was asked for, and the vendor already sees the device. This is
accepted for v1 and stated: the dialog says the pack comes from the platform
vendor and that messages are never sent; it does not claim the download is
private.

When anti-censorship is armed (ADR-0026), a pack download would leave the
device outside the tunnel. The client does not start one and says
`Language packs can't be downloaded while anti-censorship is on`; translation
and transcription in packs already on the device keep working.

Packs Lakko does not control are not deleted by Lakko (UX section 6, `Managed by
iOS`, `Managed by Android`, `Managed by your browser`).

### 5. Local storage

Translations, transcripts, detected languages and the per-conversation
choices are stored on this device only, in each client's encrypted local store,
behind the account store gate of ADR-0077, and wiped by Forget device. A cached
result is keyed on `(conversation ref, message id, last_op_id, target language)`
so an edit (a higher `last_op_id`, ADR-0023 LWW) invalidates it and a delete
for everyone removes it. They are not written to the backup, the conversation
vault (ADR-0064) or the DeviceGroup synced state in v1 (UX open question 1).

### 6. Rules written once

The decisions that the three clients must make alike go to
`lakko-ui-rules::translation`, exposed on both FFI surfaces like the other
rules:

- `translation_offered(has_text, is_own, detected, reading_languages,
  capability) -> Offer { None, Translate, TranslateAskLanguage }`;
- `conversation_bar(recent_incoming_languages, reading_languages,
  dismissed_languages, capability) -> Option<language>`, the "most of the
  recent messages" rule (window size and threshold chosen when built, recorded
  as chosen, not measured);
- `transcript_offered(has_audio, capability) -> bool`;
- language comparison by primary subtag and script, so `pt-BR` reads `pt` and
  `zh-Hant` does not read `zh-Hans` (the `lakko-l10n::match_tag` reasoning,
  applied to an arbitrary tag rather than to the interface set).

The engines themselves stay platform code; they cannot be shared.

### 7. Never in the background

The iOS NotificationServiceExtension, the Android background decrypt and the
web service worker never translate or transcribe (C-59 forbids writing, and the
NSE has no budget for a model). Notifications show the original.

### 8. Logs

No log line carries a message's text, a transcript, a translation, or a
detected or chosen language. Logs may carry the engine state (`ready`,
`needs-pack`, `unavailable`) and failure codes.

## As built (2026-10-25)

Built on branch `translation-adr-0082`; unit-tested on the three clients, no
engine has run on any device, simulator, emulator or browser, nothing walked.

- **Core and FFI.** `lakko-ui-rules::translation` (19 tests, 4 of them
  property tests) carries the four rules of section 6 plus
  `canonical_language_tag` / `same_language` / `reads_language`. One
  refinement of the UX spec: a low-confidence detection whose best guess is a
  reading language gets no `Translate` row, so short messages in the person's
  own language are not cluttered. Exposed in `translation.rs` on both FFI
  surfaces.
- **Edit staleness uses a real last op id on all three clients**
  (`lastOpId`, the DS message id of the latest edit or delete op, persisted
  with the message; a reaction does not move it). Web and iOS first used a
  hash of the text and were changed: a hash treats an edit back to the same
  words as unchanged.
- **Web.** `apps/web/src/translation/`. Translation and detection through the
  browser `Translator` / `LanguageDetector` (desktop Chrome 138+ and Edge per
  vendor docs). Transcription through `SpeechRecognition` only with
  `processLocally = true` and `available()`, fed the decoded file as a Web
  Audio track via `start(track)`. Results in IndexedDB under the device key,
  each row tagged with an HMAC of its conversation so deleting a chat deletes
  its results. Two guards turn transcription off on the device for good: a
  capture starting during a transcription, and a recognizer
  `not-allowed` / `audio-capture` error. **Residual:** a browser that has
  `processLocally` but drops the track argument would listen to the
  microphone; if the site already holds microphone permission nothing on the
  page can see it. The only defence is MDN's data that Chrome shipped
  `start(track)` (135) before `processLocally` (139).
- **iOS.** `apps/ios/Lakko/Translation/`. Translation through
  `TranslationSession` (iOS 18+; on 26.4+ with the `.lowLatency` strategy
  because the SDK says the other may use Apple Intelligence models),
  detection through `NLLanguageRecognizer`, transcription through
  `SpeechTranscriber` + `AssetInventory` on iOS 26+, else
  `SFSpeechURLRecognitionRequest` with `requiresOnDeviceRecognition` when the
  locale supports it. Apple reports neither pack size nor translation-pack
  progress, and shows its own approval sheet for a translation pack (the only
  way to obtain one). Speech permission denied is its own state
  (`transcript.permission_denied` + Open settings); the usage string is the
  catalog key `ios.permission.speech_recognition`.
- **Android.** Transcription through `createOnDeviceSpeechRecognizer` on API
  33+, the decoded file fed through `EXTRA_AUDIO_SOURCE`; a microphone capture
  starting during a transcription turns transcription off on the device for
  good (the platform documents that a recognizer ignoring the extra opens the
  microphone). **Translation: the system service first, ML Kit as the
  fallback (maintainer, 2026-10-27).** `SystemFirstTranslationEngine` uses
  `PlatformTranslationEngine` (the system
  `android.view.translation.TranslationManager`, API 31+) on a device whose
  system reports any on-device translation capability, and
  `MlKitTranslationEngine` (ML Kit Translate 17.0.3) otherwise; a device with
  a system service that lacks a pair does not fall through to ML Kit for it.
  What the fallback costs, stated: Google's ML Kit Android data disclosure
  says the Translate API reports the configured source and target languages
  for diagnostics and usage analytics, through Firebase Remote Config and
  Installations (an identifier per app installation), with no documented way
  to turn it off, so on such a device Google learns, per use, which languages
  this Lakko installation translates from and into. Text is translated on
  the device and never sent. The fallback answers nothing while
  anti-censorship is on, so no ML Kit traffic leaves outside the tunnel.
  ML Kit models are Lakko's to remove (Settings shows `Remove`); system packs
  show `Managed by Android`. ML Kit does not document whether a model it keeps
  by default appears among the downloaded ones, so a pair it confirmed in this
  session counts as ready; after a restart such a pair may ask for its pack
  again, and the download then returns at once. ML Kit Language
  Identification is not linked (it reports the identified languages); the
  source test pins both. The system service is not an SDK in the APK; its
  packs are managed by the system. Which service backs it (on Pixel devices presumably Android System
  Intelligence, not checked) and whether one exists on other devices at all is
  not known: where `getOnDeviceTranslationCapabilities` reports nothing, the
  controls are absent. Whether creating an on-device translator for a pair
  that is available to download starts the download is not documented and
  not yet seen on a device. **No platform language detection:** `TextClassifier` may run in an
  OEM-provided process, so locality cannot be confirmed; until 2026-11-15
  Translate always opened with the language picker and the conversation bar
  never appeared (since then the core detector, note of 2026-11-15).
- **Shared.** 73 new catalog keys with machine translations in the ten
  languages; security-critical ones ship in English until reviewed.
- **Not yet done:** results are not removed when a group is left or closed
  (the history stays readable there); rows of folded older history segments
  get no translation on Android; `lakko-security` not run.

## Note of 2026-11-15: text language detection from the core

Seen by the maintainer: with no detector, Translate opened the language
picker every time, and a person may not know what language a message is in.
Android had no detector by decision (section 1), and the web used the
browser's `LanguageDetector` only when its model was already on the device,
which it rarely was. iOS uses `NLLanguageRecognizer` and is unchanged.

- **Detector.** `lakko_ui_rules::language_detect::detect_text_language`, on
  the `whatlang` crate (=0.18.0, MIT, default features off; trigram profiles
  compiled into the binary, 70 languages, no network or file access, no
  `unsafe` in its source). On UniFFI as `detectTextLanguage(text:) ->
  DetectedLanguage?` (the record `translationOffered` already takes) and on
  wasm as `detectTextLanguage(text) -> DetectedTextLanguage | undefined`
  (`tag`, `confidencePermille`). The text is read on the device and is not
  stored or logged; the answer is the tag and confidence the rules of section
  6 already take, so this changes no rule.
- **Tags.** The ISO 639-1 code of each `whatlang` language, never a region;
  Iranian Persian is `fa`, Norwegian Bokmal `nb`, Tagalog `tl`, Mandarin
  `zh`. The match is exhaustive, so a new `whatlang` language fails the build
  until it is given a tag.
- **Confidence, measured.** `whatlang`'s confidence is not a probability (a
  correct one-sentence message often scores 0.1 to 0.4), so it is mapped onto
  the clients' scale: at and above a cut-off of 0.3 an answer lands at or
  above `DETECTION_CONFIDENT_PERMILLE` (700), below it under 700. Measured on
  two corpora: 534 short chat messages (28 each in nl de en es pt fr it ru uk
  pl tr ar fa he hi, 12 to 18 each in af da nb sv bg ca cs; written by the
  agent that built the module, not collected from people) and the 26 183
  interface strings of `l10n/catalog/` in 11 languages. `whatlang` picks the
  wrong language for many short messages whatever its confidence (on the
  chat corpus Italian 18 of 28 right, Spanish 19, English 21, Russian 20), so
  no cut-off makes most one-sentence messages both confident and right.
  Confident answers for the 15 major languages of the chat corpus, and wrong
  confident answers (chat / UI corpus): 0.5: 44%, 0 / 6; **0.3: 57%, 2 / 44**;
  0.2: 66%, 5 / 184; 0.15: 72%, 13 / 335. At 0.3 per language: pl 26 of 28,
  de 23, uk 22, he 22, ar 20, fa 18, fr 18, tr 18, hi 14, en 13, ru 11, es 10,
  pt 9, it 9, nl 8 (2 wrong). The cut-off is a product choice between those
  rows.
- **Rules that lower an answer below 700.** Fewer than 8 letters in a script
  several languages share; less than 80% of the letters in the detected
  language's script (mixed-script text); Chinese (Simplified and Traditional
  cannot be told apart, and a bare `zh` reads as Simplified); and letters that
  contradict the answer, each a fact about an alphabet added for a confusion a
  corpus showed: Portuguese without `ã õ ç`, Catalan without `à è ò ï ç`,
  Bulgarian without `ъ` or with `ы э ё`, Ukrainian without `і ї є ґ` or with
  `ы э ё ъ`, Belarusian without `ў`, Serbian and Macedonian without their own
  letters, Azerbaijani without `ə`, `ı` in anything but Turkish or
  Azerbaijani, Romanian without `ă â î ș ț`, Spanish with `ã õ ç`, Russian
  with `і ї є ґ ў`. Only letters reach the detector (a lone `·` read as
  Catalan at 1.0). Short messages ("ok", "hola") stay unsure and open the
  picker.
- **Cost.** A confident wrong answer translates from the wrong language,
  corrected with `Not Spanish?` (UX 3.4); the conversation bar can now appear
  on Android and on such a browser. A better detector on short text (for
  example `lingua`, Apache-2.0, whose per-language model crates are about
  2.5 to 3.3 MB each compressed on crates.io) was not evaluated.
- **Not done.** The clients are not wired yet; the binary-size cost of the
  profiles in the Android library and the web wasm is not measured.

## Alternatives

1. **A Lakko server endpoint that translates or transcribes.** Gives every
   platform the feature at once and the best quality. Rejected: it hands the
   server the plaintext of a message the requester did not write (invariant #2),
   and an opt-in only moves the disclosure to the reader's consent.
2. **A third-party cloud API called from the device** (the client sends text to
   a translation provider directly). Same disclosure to a different operator,
   plus a third party learning who reads what. Rejected.
3. **Lakko's own bundled engines on every platform** (an open speech model such
   as Whisper compiled for each target, an open translation model, packs served
   from Lakko's static host as one bundle so the fetch reveals no language, the
   ADR-0061 approach). Uniform behaviour and no vendor download. Not chosen for
   v1: the models are large, would ship or download per platform, and the
   quality and speed on mid-range phones are unknown. Kept as the path for the
   web (where most browsers have no engine) and for a later version; it
   changes no UI, only section 7's case 3 to case 1.
4. **Sender-side transcription carried in the envelope.** Recipients without an
   engine could read it; but it is the sender's claim about their own audio,
   needs a wire field and a label, and reveals nothing new only because the
   sender wrote it. Deferred (UX open question 2).

## Consequences

- **Positive:** the features people expect, with nothing new reaching any
  server; no wire, schema, service or `lakko-mls` change; the rules the three
  clients must agree on are written once.
- **Negative:** availability differs by platform and version: iOS
  transcription below iOS 26 depends on `SFSpeechRecognizer`'s on-device
  support for the language; Android on-device recognition depends on the
  device having an on-device recognition service; on the web only desktop Chrome
  translates, and transcription depends on `processLocally` support, so most
  browsers get neither. The vendor learns which language packs a device
  fetched. On an Android device without a system translation service, ML Kit
  tells Google which languages are translated, at every use. Quality is the
  platform's, and Lakko cannot improve it.
- **Neutral:** translations and transcripts exist on one device only in v1, so
  a person reading on two devices translates twice.

Invariants: #2 untouched (no new server-visible data), #4 gains the
local-only corollary of section 2, #5 untouched. `lakko-mls` untouched
(invariant #3). Not a crypto change; a `lakko-security` pass is optional and
would look at section 2 and section 5.

## Open questions

1. Sync of results and per-conversation choices to the account's other devices
   (UX open question 1). Decided for v1 (2026-10-25): not synced; may be
   revisited after v1.
2. Sender-attached transcripts (UX open question 2). Decided for v1
   (2026-10-25): none; may be revisited after v1.
3. Own bundled engines (alternative 3): when, and which models, measured on
   which devices.
4. Whether Chrome falls back to its server engine when `processLocally = true`
   cannot be honoured, or refuses: the draft says it must be local; behaviour in
   shipping Chrome is to be checked before the web client relies on it.

## References

- Apple, `TranslationSession` documentation (on-device processing, iOS 18+).
- Apple, WWDC25 session 277, SpeechAnalyzer (on-device, model assets).
- Android, `SpeechRecognizer` reference (`createOnDeviceSpeechRecognizer`,
  `isOnDeviceRecognitionAvailable`, API 31; the remote-streaming note).
- Google, ML Kit Android data disclosure (the Translate API reports the configured source and target languages), the cost of the fallback.
- Android, `android.view.translation.TranslationManager` reference (API 31).
- Chrome, Translator and Language Detector APIs (Chrome 138, desktop).
- MDN, `SpeechRecognition.processLocally`; Chromium "Intent to Ship:
  On-device Web Speech API".
- ADR-0047, ADR-0052, ADR-0061, ADR-0064, ADR-0077.
