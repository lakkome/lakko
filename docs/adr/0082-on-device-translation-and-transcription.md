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
  "As built"); since 2026-11-15 per language pair, ML Kit for every pair
  whose pack the system does not already have (note of that date). **Amended 2026-11-15:** text language detection comes from the
  Rust core where the platform has none Lakko can use (Android always; the web
  when the browser's `LanguageDetector` is absent or its model is not on the
  device); iOS keeps `NLLanguageRecognizer` (note of 2026-11-15).
  **Amended 2026-11-18 (maintainer):** where the platform has an on-device
  engine for a language it is used, otherwise an on-device engine Lakko
  ships (unbuilt); the macOS desktop app runs Apple's engines through a host
  helper (note of 2026-11-18).
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
| Android | `SpeechRecognizer.createOnDeviceSpeechRecognizer` (API 31+), only after `isOnDeviceRecognitionAvailable` returns true. `createSpeechRecognizer` is never used: its documentation says the implementation is likely to stream audio to remote servers | The system `android.view.translation.TranslationManager` (API 31+), only `createOnDeviceTranslator` and `getOnDeviceTranslationCapabilities`, never the deprecated `createTranslator`; on a device whose system reports no on-device translation capability (or below API 31), ML Kit Translate, whose data disclosure says it reports the configured source and target languages to Google (amended 2026-10-27; never while anti-censorship is on). Since 2026-11-15 the choice is per language pair: the system service for a pair whose pack it already has, ML Kit for every other pair, its pack download included, because Lakko has no documented way to make the system download one | No platform detector: `TextClassifier` may run in an OEM process and ML Kit Language Identification reports the identified languages. Since 2026-11-15 the core detector `detectTextLanguage` (note of that date) |
| Web | `SpeechRecognition` only with `processLocally = true`, which the draft specification defines as "must be done locally"; with it absent or unsupported, the feature is absent. Experimental; which browsers honour it is not established | `Translator` API (Chrome 138+, desktop only per Chrome's documentation; Firefox and Safari not supported) | `LanguageDetector` API (same availability), only with its model already on the device; otherwise, since 2026-11-15, the core detector `detectTextLanguage` (note of that date) |
| watchOS | none in v1 | none in v1 | none |
| Desktop app (ADR-0085) | macOS 26.4+: Apple's `SpeechAnalyzer` + `SpeechTranscriber` through the host's helper (note of 2026-11-18); Windows, Linux: none yet | macOS 26.4+: `TranslationSession` through the helper; Windows, Linux: none yet | macOS: `NLLanguageRecognizer` through the helper, then the core detector; elsewhere the core detector |

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
  a system service that lacks a pair does not fall through to ML Kit for it
  (superseded by the note of 2026-11-15: the choice is per pair).
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
- **Source order and the language line (later the same day, maintainer).**
  A conversation's remembered source (UX 3.4) is used only when there is no
  usable guess: a remembered Dutch pick, seen live on web, read an unsure
  Ukrainian message (`uk` at 0.67) from Dutch. The order on all three clients
  is a source chosen for this message, a confident detection, the guess even
  unsure (when not the target language and not one the engine cannot
  translate from), the conversation's source, else the picker. And an
  incoming message whose detected language is not a reading language now
  shows the quiet line `Spanish · Translate` up front (UX 3.2), decided by
  `translation_offered` asked with the engine fact set to "can translate";
  the engine's answer for the guess only decides the `Translate` button
  (absent when the engine cannot translate from the language). No rule
  change in the core. Unit-tested only, not walked.
- **Close candidates and the conversation's language (later the same day,
  maintainer).** Seen on a Pixel: "hola, qué tal?" was labelled Catalan and
  "goedendag" Turkmen, both unsure, both on a name-only line because the
  device cannot translate from either. Restricting the detector up front to
  the languages the engine translates from was rejected: a real message in a
  language the device neither translates nor the person reads (Turkmen) would
  be relabelled as a candidate (Turkish) and translated wrongly. What was
  built instead:
  - *Candidates, not one winner.* A detection carries up to three close
    candidates beside the top language
    (`lakko_ui_rules::language_detect::detect_text_language_candidates`):
    languages whose `whatlang` score is within 0.030 of the top one, nearest
    first, with the letter-contradiction rules applied to each; a confident
    detection has none. Measured scores on the cases seen: "hola, qué tal?"
    Catalan 0.737, Spanish 0.729, French 0.715, Italian 0.710; "Ciao, come
    stai?" Spanish 0.739, Portuguese 0.736, Italian 0.735; "goedendag"
    Turkmen 0.748, Danish 0.739, Dutch 0.737; against confident cases
    "¿Qué tal? ¿Cómo estás?" Spanish 0.688 vs Portuguese 0.540. The margin
    (`CANDIDATE_SCORE_MARGIN_PERMILLE` = 30) and the cap of three are chosen,
    not measured: those cases are the only data. On the chat corpus 252
    messages are unsure; the top language is right for 156 of them and the
    right language is in the set for 231 (set sizes 1: 45, 2: 102, 3: 54,
    4: 51). The ranked list comes from `whatlang::dev::raw_detect`, which
    upstream calls internal and exposes only behind its `dev` feature;
    `whatlang` is pinned exactly (=0.18.0) and the measured cases are pinned
    in tests. A platform detector that reports probabilities (iOS
    `NLLanguageRecognizer.languageHypotheses`, the browser `LanguageDetector`)
    goes through the same rule, `candidates_from_hypotheses`, which keeps
    languages at least half as probable as the top one
    (`CANDIDATE_PROBABILITY_RATIO_PERMILLE` = 500, chosen). On UniFFI and wasm
    as `detectTextLanguageCandidates` and `languageCandidatesFromHypotheses`.
  - *The conversation's languages pick among close candidates.* Per
    conversation and per device (beside the picker's choices), Lakko keeps
    the languages the person picked (`Not X?` or the picker), the latest
    time per language, and each message's confident detection, once per
    message id (`lakko_ui_rules::language_detect::ConversationLanguages`;
    the last 200 message ids and 16 picked languages are kept, both chosen,
    not measured). For an unsure detection, `conversation_detection` takes
    among its candidates a picked language, the most recently picked first;
    else the candidate with the most confident detections in the chat; a
    tie, or none, goes in the detector's candidate order. A confident
    detection is never changed. The record's form is canonical (sorted,
    bounded by message id, not by arrival), so the same detections in any
    order give the same answer (a proptest pins it). The first build kept a
    single remembered language overwritten by whichever confident detection
    ran last, and a chat with sure Spanish, Italian, Ukrainian, German and
    Russian messages named "hola, qué tal?" Spanish on one launch and Italian
    after a reinstall, as rendering and scrolling reordered the detections;
    it now reads Spanish (Spanish and Italian once each, Spanish first among
    the candidates). The `Translate`-only fallback is the usual language:
    the latest pick, else the most detected one (a tie by tag). On UniFFI as
    `ConversationLanguages` / `conversationLanguagesWithPick` /
    `conversationLanguagesWithDetection` / `conversationLanguagesUsual`, on
    wasm as the `ConversationLanguages` class.
  - *The line names what `Translate` uses.* One rule decides both:
    `resolve_translation_source(candidates, remembered, target,
    translatable)` (UniFFI and wasm `resolveTranslationSource`) returns the
    language the line names, whether it carries `Translate`, the source
    `Translate` starts from and the guess the picker marks. In order: a
    confident detection (named alone when the device cannot translate from
    it, so a real Turkmen message still reads `Turkmen`); the conversation's
    language among the close candidates; the guess, when the device can
    translate from it; the first other close candidate it can translate
    from; for `Translate` only, the conversation's usual language; otherwise
    the picker. `translatable` is the client's engine answers for every
    candidate and the usual language. The only message with no line is
    an unsure one with no candidate the device can translate from. The first
    build of this note hid every unsure guess the device cannot translate
    from, which on a Pixel left "hola, qué tal?" with no line although
    `Translate` went to Spanish; the line now reads `Spanish · Translate`.
    iOS also puts a language chosen for one message ahead of the rule.
  - *`Not X?` lists the close candidates first,* then every other language in
    the usual order (`order_picker_languages`).
  - A second detection restricted to given languages was built, measured (for messages in a
    19-language list resembling the platform translators' the right
    language named 298 -> 333 of 364, and no confident answer for the 170
    messages outside it) and wired as a tap-time second opinion; it was
    removed the same day from the core, both FFI surfaces and the three
    clients in favour of the close candidates, which come from the same
    detection, need no list of what an engine supports, and are what
    `Not X?` shows.
  - Residuals: when the device can translate from Catalan, "hola, qué
    tal?" is named and translated as Catalan unless the conversation's
    language is Spanish; "Можешь мне позже перезвонить?" is an
    unsure Ukrainian (0.427) with no Russian alternative, Russian falling
    outside the margin.
  - iOS availability, checked after names without `Translate` were seen on an
    iPhone for Italian, German, Ukrainian and Dutch while Spanish showed it:
    the code asks `LanguageAvailability.status(from:to:)` per pair with
    `Locale.Language(identifier:)` on canonical tags and maps `installed` to
    ready, `supported` to a pack to download (still offering `Translate`)
    and `unsupported` to nothing. On this Mac (macOS 27.2) every pair among
    es it de uk nl fr pt ru into en, ru, en-US, uk, es, tr, he, pt-BR,
    zh-Hans, ar and hi answered `installed`; `unsupported` came only for a
    pair into its own language and for the target fa. No defect was found in
    the pair or the target tag; the iPhone's cause is not established.
    The iOS 27.2 simulator cannot reproduce it: it lists 38 supported
    locales yet answers `unsupported` for every pair asked, bare and regional
    tags alike (es>en, es-ES>en-US, it-IT>en-US, en-US>es-ES) and for
    `status(for:to:)` on whole sentences, so it has no translation service to
    ask. `TranslationAvailabilityTests` in `LakkoTests` asks each pair of
    es it de uk nl fr into en and ru and fails on `unsupported`; it skips
    where even en>es is unsupported, so it says something only when run on a
    device.
  - **Correction (later the same day, on the iPhone, iOS 27.2).** The
    "names without `Translate`" were a reading error: the screen-reading
    script dropped repeated labels, so every `Translate` after the first was
    missing from its output; the raw tree had one under each message. The
    real iOS defect was behind the button: every `Translate` ended in
    "Couldn't translate". `LanguageAvailability()` (default strategy)
    answered `installed` for it/de/es/nl into en, while the session, built
    with the `lowLatency` strategy, threw `TranslationError.notInstalled`;
    a session with the default strategy translated. So the availability is
    now asked with `LanguageAvailability(preferredStrategy: .lowLatency)` on
    iOS 26.4+, the strategy the sessions use: the pair then reads as a pack
    to download. Seen on the iPhone: `Translate` on an Italian message
    offered "Download Italian"; Apple's sheet downloaded Italian and English
    (US); the message was translated. `lowLatency` is kept on purpose (the
    traditional on-device model, never the Apple Intelligence path).

  Built on the core, both FFI surfaces, web, iOS and Android; unit-tested
  only, not walked.
- **Android: ML Kit for every pair the system service cannot supply (later
  the same day, maintainer).** Seen on a Pixel 9 (API 35): `Translate` on a
  Spanish and on a Danish message opened the pack dialog; `Download` created
  and destroyed a system translator for the pair, polled the capability for
  120 s and ended `scheduled`, and after more than three minutes both lines
  still read `Downloading… · Cancel` with nothing watching them. Creating a
  translator does not make the system download a pack, and Lakko has no
  documented call that does, so on a phone with the system service no pack
  could be obtained through Lakko. Decision: `SystemFirstTranslationEngine`
  chooses per language pair. A pair the system answers READY for is the
  system's; every other pair (system NEEDS_PACK, or UNAVAILABLE for a
  language ML Kit has, such as Ukrainian) goes to `MlKitTranslationEngine`:
  its state, its pack download and its translation. ML Kit reports no
  download progress, so the line shows none. `PlatformTranslationEngine`
  no longer pretends to download (DONE only when the pair is on the device,
  else FAILED). ML Kit's rules stay: nothing while anti-censorship is on
  (such a pair reads as unavailable and the line follows the usual rule;
  packs already on the device keep working), Wi-Fi only unless the person
  allowed mobile data, the dialog says the pack comes from Google, its packs
  are listed in Settings with Remove. A download that ends `scheduled` no
  longer leaves the line on `Downloading…`: it returns to the line the
  message had (`Spanish · Translate`), and the next tap asks the engine
  again; a transcription returns to closed. Privacy cost, stated: on a
  phone with the system service, any pair it lacks now goes through ML Kit,
  so Google learns, per use, the source and target languages this
  installation translates (never the text), where before only devices
  without the service paid that. Host-tested only (fakes for the system and
  ML Kit, the real ML Kit engine with anti-censorship on); not run on a
  device.

## Note of 2026-11-18: the system engine where there is one, Lakko's own where there is none; the desktop app on macOS

**Decision (maintainer, 2026-11-18).** On every client, a language is
translated and transcribed by the platform's own on-device engine when it has
one for that language; where it has none, by an on-device engine Lakko ships
itself (alternative 3 below: Lakko-served packs, the ADR-0061 approach). That
covers Windows and Linux, browsers without `Translator` or local
`SpeechRecognition`, and languages Apple or Google do not cover. Nothing about
section 2 changes: every engine in the order runs on the device, and with
none for a language the control is still not offered. **Lakko's own engine is
not built**; the order exists in code so that it joins without touching a
screen.

**The order, as built on the web client.** `apps/web/src/translation/
engine-order.ts` takes a list of engines; per language (per pair for
translation) the first one that does not answer `unavailable` owns it, and its
state, its pack and its result are the ones used. A system pack not yet on the
device (`needs-pack`) still belongs to the system. `device-engines.ts` builds
the list: the desktop host's system engines, then the browser's; Lakko's own
engine is meant to go last. The controller and the screens read one engine set
and do not know there is a list.

**The desktop app on macOS.** WKWebView has no `Translator`, no
`LanguageDetector` and no local `SpeechRecognition`, so until now the desktop
app said both features were not available. The host now runs Apple's engines
in a helper next to it, `lakko-engines` (`apps/desktop/macos-engines/
main.swift`, built and placed in `Lakko.app/Contents/MacOS` by
`just dev-desktop`), spoken to over its standard input and output, one JSON
object per line (`apps/desktop/src-tauri/src/engines.rs`, routes
`/__host/engines/*`). A helper rather than FFI because the host crate forbids
`unsafe`. The engines are the iOS client's:

| | Engine on macOS | Local guarantee |
|---|---|---|
| Detection | `NLLanguageRecognizer`, its ranked list through `languageCandidatesFromHypotheses` (the shared rule), then the core detector | runs in process |
| Translation | `TranslationSession(installedSource:target:preferredStrategy: .lowLatency)`, state from `LanguageAvailability(preferredStrategy: .lowLatency)` | an `installedSource` session uses packs already on the Mac and never starts a download; `lowLatency` is never Apple Intelligence |
| Transcription | `SpeechAnalyzer` + `SpeechTranscriber`, installed state from `SpeechTranscriber.installedLocales`, packs through `AssetInventory` | on-device by Apple's documentation |

- **Requires macOS 26.4** (the `lowLatency` strategy); the helper is built for
  it and does not load on anything older, which the host reports as no
  engines. The host runs `lakko-engines --probe` at start (about 0.16 s,
  measured on this Mac, macOS 27.2) and hands the page a read-only global,
  `__LAKKO_SYSTEM_ENGINES__`, before any page script runs.
- **What crosses, and where it stays.** The page sends the host the text of a
  message to detect or translate, and the audio of a voice or video message,
  decoded in the page by Web Audio and resampled to 16 kHz mono 16-bit PCM;
  the host relays them to the helper over a pipe. Nothing is written to disk
  (the iOS client exports an M4A to a temporary file; the helper takes PCM
  buffers instead) and nothing is logged: the host logs the operation and
  whether it worked, the helper writes nothing but its answers. Results are
  stored by the web client as before (section 5, behind the ADR-0077 gate).
- **Translation packs: managed by macOS.** Apple offers no way for a program
  without a SwiftUI window to download a translation language
  (`translationTask` / `prepareTranslation` are SwiftUI-only). So the pack
  dialog (UX 3.5) says that macOS downloads it in System Settings, Language
  & Region, Translation Languages, and its button opens that pane
  (`x-apple.systempreferences:com.apple.Localization-Settings.extension`).
  Lakko downloads nothing and waits for nothing: the line returns to
  `Spanish · Translate` and the next tap asks macOS again.
- **Transcription packs** download through `AssetInventory` from the helper,
  after the person agreed, as on iOS.
- **Anti-censorship.** The desktop app is the first web-based client with
  anti-censorship (ADR-0085). While it is switched on, no pack is offered: the
  host answers `packs-blocked`, and the line says `Language packs can't be
  downloaded while anti-censorship is on` (the web client gained this state;
  the browser has no anti-censorship and never shows it).
- **Measured on this Mac (macOS 27.2.1, 2026-11-18), with unbundled test
  programs:** no translation pack was installed, and es, ru, de, fr to en
  and en to ru all answered `needs-pack`; an English recording, fed to the
  transcriber as an in-memory buffer, came back word for word with no
  speech-recognition permission prompt (the authorization status stayed
  not determined); `AssetInventory.status` answered `supported` for an
  English model that was on the Mac and transcribed, so installed is read
  from `installedLocales` (the iOS client reads `status`; whether iOS has the
  same discrepancy is not checked); Russian has no `SpeechTranscriber` locale;
  with standard input read on the main thread `LanguageAvailability` never
  answered, so the helper reads on a thread of its own.
- **Not built:** Windows and Linux (the host reports no engines there, and
  the page says translation and transcription are not available on this
  device); an Intel build of the helper (arm64 only); partial transcripts
  while the helper works (the line says `Transcribing…` until the end).

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
3. Own bundled engines (alternative 3): decided 2026-11-18 as the engine for
   every language the platform has none for (note of that date); which
   models, measured on which devices, is open.
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
