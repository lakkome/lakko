<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# ADR-0100: Lakko's own on-device translation engine for the web client and the desktop app

- **Status:** Accepted (2026-11-25, maintainer): Bergamot after the S1
  measurement in section 1, all 114 directions, groups of three languages
  by region, no persistence request, Mozilla's MPL-2.0 statement relied on.
  S2 built 2026-11-25 (engine, worker, downloader, manifest tool, dialog
  and settings, desktop forwarding, nginx config); unit-tested and checked
  in Firefox, Opera and Safari on a local dev server and in the macOS
  desktop app (2026-11-26); the dialog and Settings walked signed in on a
  local stack (2026-11-26); deployed to test.lakko.me 2026-11-26 (models,
  nginx, web build of 4a1982462).
- **Date:** 2026-11-25
- **Context:**
  - [ADR-0082](0082-on-device-translation-and-transcription.md), note of
    2026-11-18: a language goes to the platform's on-device engine where
    there is one; otherwise it goes to an on-device engine Lakko ships.
    That second half is unbuilt.
  - UX spec [`docs/ux/translation-and-transcription.md`](../ux/translation-and-transcription.md),
    sections 3.1, 3.5, 6 and 7.
  - `apps/web/src/translation/engine-order.ts` and `device-engines.ts`,
    which already leave a place at the end of the list for this engine.

## Context

On 2026-11-25 the maintainer opened test.lakko.me in Opera. Messages in
Spanish, Italian and Russian showed their language name and no `Translate`
control. This is UX 3.1 case 3: a sure detection and no engine. The web
client's only translator is the browser's `Translator` API.

- **Who has `Translator`.** MDN browser-compat-data 8.1.10 (2026-11-19) lists
  it for Chrome desktop 138, Edge 148 and Opera 122. It lists nothing for
  Firefox, Safari, Chrome on Android, Safari on iOS or Samsung Internet.
  - The Opera row may have been copied from Chrome's data. What a person saw
    in Opera says the API is absent there, or does not offer these pairs.
  - Mozilla's standards position on the Web Translation API is `negative`
    (mozilla/standards-positions#1015). WebKit's (#339) is open, with no
    position. Nothing indicates that either engine will ship one.
- **The desktop app.** It loads the same web build. On macOS 26.4+ it uses
  Apple's engines through the host helper. On Windows, Linux and older macOS
  it has no translator at all.

So the fallback engine ADR-0082 decided on is what decides whether most
people on the web, and every desktop user off macOS 26.4, can translate at
all. Transcription is not part of this ADR (open question 7).

### Constraints from the code as it is

- **Models must come from Lakko's own origin.** Both CSPs (production
  `lakko-security-headers.inc`, test stack `lakko-test-security-headers.inc`)
  set `connect-src 'self'` plus the SFU socket and `script-src 'self'
  'wasm-unsafe-eval'`. A model can therefore be fetched from Lakko's own
  origin only, unless the CSP is widened. Widening it would also send every
  model request, with the reader's IP address, to a third party.
- **No threads.** Production sends `Cross-Origin-Embedder-Policy:
  unsafe-none`, so the page is not cross-origin isolated and has no
  `SharedArrayBuffer`. A threaded wasm build is unavailable without changing
  that header. It is not needed (section 1).
- **Large wasm assets have precedent.** The web build already serves
  `lakko_segment.wasm` (17 MB in `apps/web/wasm/` as built here) and
  `lakko_noise.wasm` through Vite `?url` imports, and nginx has a `.wasm`
  location.

## Decision

**The web client runs Mozilla's Bergamot inference engine (Marian), compiled
by Lakko to WebAssembly, in a Web Worker. It runs Mozilla's Firefox
Translations models, mirrored on Lakko's own static host. The engine joins
`engine-order.ts` last. Nothing about a message leaves the device. The model
download tells Lakko's server which language pair this device asked for, and
the pack dialog says so.**

### 1. Engine

The inference engine is the `inference/` directory of `mozilla/translations`.
It is MPL-2.0, maintained (last push 2026-11-19), and the source of the wasm
Firefox ships.

- **The old repository is not used.** `browsermt/bergamot-translator` and
  its npm package `@browsermt/bergamot-translator` (0.4.9, published
  2022-10-03, last push 2024-05-12) are not used.
- **Lakko builds the wasm itself.** It is built from a pinned commit with
  the emsdk pinned in that repository, as `lakko_translate.wasm`, by a
  script beside `build:noise` / `build:segment`. Lakko's own JS glue drives
  it from a dedicated worker (`apps/web/src/translation/lakko-engine/`).
  - Why Lakko builds it: the binary that runs is then the source the
    repository names, and the build is reproducible.
  - For scale: Firefox's own build of this engine (`translations-wasm-v2`,
    version 4.0) is 4,960,506 bytes decompressed and 1,211,955 bytes
    zstd-compressed. Lakko's build will differ and will be measured.
- **What the browser must provide.** Fixed-width WebAssembly SIMD: both
  CMake files compile with `-msimd128`. MDN lists SIMD since Chrome 91,
  Firefox 89, Safari 16.4 (iOS 16.4), Opera 77 and Samsung Internet 16.
  Without SIMD the engine reports `unavailable` for every pair.
- **Threads are not used.** The bergamot 0.4.9 glue gets its parallelism
  from separate workers, each with its own model copy, not from
  `SharedArrayBuffer`. That is inferred from its code, and is checked again
  on the Lakko build in S1. One worker is used, so COEP stays as it is.
- **Matrix multiply.** In a web page the engine always uses its wasm
  fallback. The `mozIntGemm` intrinsic exists only in Firefox's privileged
  extension context (bergamot README).
  - The only published speed figures are Mozilla's, for Firefox, not for a
    web page:
    - bug 1746631, Firefox 98, a 2017 MacBook Pro i7: the fallback ran
      95 words/s against 560 for native AVX2 on en->de, and 105 against 625
      on en->es;
    - bug 1926100, x86_64 Linux, en->ru: 420.92 words/s on the fallback
      against 786.66 with `mozIntGemm` (~20 MB model), and 237.79 on the
      fallback with a ~40 MB model.
  - **S1, measured 2026-11-25** on this Mac (Apple silicon, macOS 27.2) in
    a web page served from localhost, one Web Worker, the fallback gemm,
    the v2 models of es->en (`base-memory`) and en->ru (`base`), 20 chat
    sentences translated twice; times from the second pass:

    | Browser | Pair | Model load | Median per sentence | Words/s |
    |---|---|---|---|---|
    | Firefox | en->ru | 230 ms | 27 ms | 415 |
    | Firefox | es->en | 198 ms | 22 ms | 501 |
    | Opera | en->ru | 97 ms | 24 ms | 448 |
    | Opera | es->en | 120 ms | 20 ms | 544 |
    | Safari | en->ru | 141 ms | 24 ms | 493 |
    | Safari | es->en | 115 ms | 18 ms | 581 |

    - Wasm linear memory after the run: 253 MiB (Opera, en->ru) and
      196 MiB (Firefox, es->en). Load time excludes the download.
    - The build was a scratch spike, not the repository build of S2.
    - en->ru output had malformed words in a few of the 20 sentences
      ("приезшь", "пекарю", "был ли оплачено счет"). Whether Firefox's own
      translator produces the same output from the same model is not
      checked.
- **As built (2026-11-25), the engine binary.** `tools/translate-wasm/build.sh`
  builds `apps/web/wasm-translate/lakko_translate.{mjs,wasm}` (5.0 MB wasm, 1.8 MB
  gzip) from the pinned commit with emsdk 3.1.8 and zstd 1.5.7 (hash
  checked), as an ES module for a module worker, `-sDYNAMIC_EXECUTION=0`
  for the CSP; two clean builds are byte-identical. Two things differ from
  Mozilla's build:
  - **The matrix multiply is linked inside the wasm.** Upstream routes
    every int8 matrix multiply wasm -> JS -> wasm through a JS wrapper
    (`import-gemm-module.js`) so Firefox can swap in `mozIntGemm`, which a
    web page never has. In an ES module that wrapper runs in strict mode,
    and under JavaScriptCore (Safari 27.2, this Mac) a strict JS function
    re-entering wasm on that path gave wrong results after the first
    sentence or two: 37 of 40 translations empty, against 0 of 40 for the
    same objects linked as a classic script, and 0 of 40 in Firefox and
    Opera. Bisected to that one function. `gemm_direct.cpp` binds the
    seven `wasm_gemm` symbols to their fallback implementations inside the
    module; after it Safari, Firefox and Opera give 40 of 40, byte-identical
    to Mozilla's own build.
  - **zstd is compiled in**: `zstdDecompressToAligned(src, alignment,
    expectedSize)` decompresses a model file straight into aligned engine
    memory. Mozilla's frames state no size, so the manifest's is required
    and the output must be exactly that long.
- **As built, workers.** The page runs one worker per loaded direction (at
  most two, chosen) and translates a pair through English one direction
  after the other, as Bergamot's own `pivotMultiple` does. (This was made
  while the Safari defect above was misread as two models in one instance;
  it is kept because it holds one model per engine instance in every
  browser. Cost: a second engine instance during a pair through English.)
- **Verified 2026-11-25** in Firefox, Opera and Safari on this Mac through
  the web client's own engine code on a local dev server (models from a
  local directory with byte ranges, as nginx serves them): groups of 147.4
  and 140.7 MB downloaded, checked, stored; es->en, en->ru and es->ru
  through English translated; a second translation reuses the loaded
  worker. **2026-11-26 in the macOS desktop app** (debug build, WKWebView,
  anti-censorship off, models from a local origin through the host's
  forwarding of `/translation-models/<hex>`): the pl-cs-sk and ru-uk-be
  groups (135.3 and 140.7 MB) downloaded in 4 MiB ranges, checked and
  stored; cs->en, en->ru and cs->ru through English translated, a second
  cs->en reused the loaded worker, and both groups were listed as kept.
  **2026-11-26, signed in on a local stack** (two accounts, web client in
  the Claude app's Chromium 156 pane, models from a local directory): a
  Czech message offered Translate; the dialog named the pl-cs-sk group and
  135.3 MB; Download translated it into English; Settings listed the group
  as `Translation · 135.3 MB` with Remove; after Remove the next Translate
  asked again, and `Not now` left the message untranslated; with Russian as
  the target the dialog named both groups (276.0 MB), showed
  `Translating…`, then the Russian text; Forget device deleted the
  `lakko-translation-models` database. Found on the way: in that Chromium
  `Translator.availability()` and `LanguageDetector.availability()` never
  settle, and the client waited for them without a bound, so Translate was
  never offered and Lakko's engine never asked. Since then the browser
  adapters (`browser-engines.ts`) take a browser that has not answered
  within 3 s (chosen, not measured) as unavailable and stop asking it until
  the page reloads. The dialog's source sentence said "The pack comes from"
  even for two packs; it now reads "Packs come from Lakko's server" in all
  11 languages.
- **Deployed 2026-11-26 to test.lakko.me:** the 19 group files (2.53 GB,
  every file's SHA-256 equal to its name) in
  `/var/www/lakko-translation-models`, the nginx location, and the web
  build of `4a1982462` from a clean worktree (services not restarted).
  Checked on the server: a range request answers 206 with the right
  `Content-Range`, a missing group 404 with no cache header (it carried
  the year-long `immutable` header until the `always` was removed), and,
  on a page of test.lakko.me under its CSP, the deployed worker loaded
  is-en from eight 4 MiB range requests with every hash matching and
  translated an Icelandic sentence. Found on the way: the mirror recipe
  used `rsync --chmod`, which macOS's openrsync refuses; it now sets the
  mode over ssh after copying. Not walked: the signed-in screens on
  test.lakko.me, and the desktop app through the anti-censorship tunnel
  (an isolated copy was built and armed the tunnel; the screens were not
  driven).
- **Output is plain text.** The engine's HTML mode is off. Output is
  rendered as text like every translation today (ADR-0082 section 5). This
  answers the injection concern raised in mozilla/standards-positions#1015.

### 2. Models

The models are Mozilla's Firefox Translations models: Remote Settings
collection `translations-models-v2`, read 2026-11-25, last modified
2026-10-06.

- **Licence.** The `mozilla/translations` README states: "The model files
  are distributed under the MPL 2.0 license."
  - MPL-2.0 section 1.12 names the AGPL-3.0 as a Secondary License. Section
    3.3 permits distributing a Larger Work that combines MPL code with an
    AGPL work, as long as the MPL requirements are kept for the MPL files:
    notices, and the source form of what was built.
  - Lakko keeps the MPL notices, annotates the model files and the wasm in
    `REUSE.toml`, and names the source commit.
  - **The licences of the training corpora behind the weights are not
    stated by Mozilla anywhere this ADR could find. That is open question 5.**
- **Every pair goes through English.** All 114 desktop directions pair a
  language with English; Firefox's documentation says it pivots on `en`,
  one hop at most. A pair such as es->ru therefore needs two models, es->en
  and en->ru. It takes two downloads and two passes, and the quality is
  that of two translations.
- **Sizes, read from the collection (`attachment.size` /
  `decompressedSize`).**
  - The set a desktop browser would use (records with no filter, or the
    `OS != Android` filter, highest version per direction) is 114
    directions. Together that is 2.53 GB zstd-compressed and 3.79 GB
    decompressed.
  - One direction is 13.5 to 48.2 MB compressed, median 24.0 MB.
  - By architecture, over records with no filter:
    - `tiny`: 43 directions, median 14.4 MB compressed / 22.0 MB
      decompressed;
    - `base-memory`: 75 directions, median 24.5 MB / 36.5 MB.
  - Measured for one pair (es->en, v1 collection files, hashes matching the
    manifest): 37,014,089 bytes raw, 26,172,674 with `gzip -9`, and
    24,537,787 with brotli quality 11.
- **The ADR-0061 approach does not fit.** ADR-0061 serves every language in
  one bundle so that the fetch reveals nothing. At 2.53 GB it is not an
  option (alternative 2).
- **Interface languages are all covered** in both directions to English:
  - `base-memory`: ar, es, pt, ru (en->ru `base`, 31.1 MB);
  - `tiny`: fa, he, hi, tr;
  - uk->en `tiny` and en->uk `base-memory`;
  - zh-Hans->en `base` (47.6 MB) and en->zh-Hans `base-memory`.
- **The v1 set is open question 1.** The proposal is every direction
  Mozilla ships without a channel filter. That is the same set a person gets
  in release Firefox.

- **Licences of what is compiled in (build of 2026-11-25,
  `tools/translate-wasm/THIRD_PARTY.md`).** From the link line: the
  `inference/` code MPL-2.0; marian-fork, intgemm, yaml-cpp, phf, cnpy and
  onnxjs MIT; sentencepiece (with the Abseil subset) Apache-2.0;
  protobuf-lite BSD-3-Clause; pathie-cpp BSD-2-Clause; zstd BSD-3-Clause;
  the Emscripten runtime MIT / NCSA (musl MIT, libc++ Apache-2.0 with the
  LLVM exception). PCRE2 and ruy are not in the wasm build. Eigen (included
  by onnxjs's sgemm) counts as MPL-2.0: its three LGPL-2.1 files are
  sparse-solver headers `Eigen/Dense` does not include, and the build
  defines `EIGEN_MPL2_ONLY` so including one fails it (the rebuild with it
  produced a byte-identical `.wasm`). marian-fork's bundled
  onnx protobuf (no licence file) is not in the binary: its sources are
  wrapped in `#ifdef USE_ONNX`, which this build does not define. The binary
  is not committed; it is built.

### 3. Serving and integrity

- **Where the files live.** Under one path on Lakko's own origin, e.g.
  `/translation-models/<file sha256>`.
  - They are immutable and named by hash, served with a long
    `Cache-Control`.
  - They carry no cookie, session or token, so no account is attached to
    the request.
  - The nginx `access_log` is off for that location, as for the GIF proxy
    (ADR-0047).
  - The files are copied from Mozilla's CDN by an operator tool,
    `tools/translation-models` (fetch, check Mozilla's `hash` and
    `decompressedHash`, write). They are never fetched from Mozilla by a
    client.
- **What a model is checked against.** A model manifest that is part of the
  web build itself (`apps/web/src/translation/lakko-engine/models.json`):
  per direction, the files, their compressed and decompressed sizes and
  their SHA-256 hashes.
  - After download and decompression, the worker loads a file only if its
    hash matches the manifest. Otherwise the pair stays `needs-pack` and the
    line shows the ordinary download failure.
  - On the web the manifest arrives with the code from the same server, so
    it adds nothing against the server. It does protect against the cache
    or the disk being tampered with.
  - In the desktop app the manifest is inside the signed install and the
    models come from the network. There it is what stops a server from
    handing the app a model of its choosing.
  - A model update ships with a web deploy, like a translation fix on the
    web (ADR-0061 section 5, "Web gets no bundle").
- **The signed-bundle form of ADR-0061 is not used.** A separately signed
  manifest would only matter if models were updated between releases. That
  is alternative 4.
- **Decompression.** The files are zstd-compressed on Mozilla's CDN. Lakko
  either keeps them as zstd and decompresses in the worker (a wasm zstd
  decoder inside `lakko_translate.wasm`), or recompresses them once with
  brotli in `tools/translation-models` so the browser's `Content-Encoding`
  does it. That is open question 6.

### 4. Language groups: what the download reveals, and what the dialog says

**Languages come in groups of three, by region** (maintainer, 2026-11-25).
A group is the six directions of its three languages, each to and from
English. It is served as **one file**, so one request names a group and
never a language. The 57 languages of the 114 directions form 19 groups
(sizes: the sum of Mozilla's compressed files, from the collection):

| Group | Languages | MB |
|---|---|---|
| 1 | Russian, Ukrainian, Belarusian | 140.7 |
| 2 | Polish, Czech, Slovak | 135.3 |
| 3 | Serbian, Croatian, Bosnian | 145.4 |
| 4 | Bulgarian, Slovenian, Romanian | 125.1 |
| 5 | Lithuanian, Latvian, Estonian | 126.1 |
| 6 | Swedish, Danish, Norwegian Bokmal | 86.8 |
| 7 | Finnish, Icelandic, Hungarian | 144.8 |
| 8 | German, Dutch, Afrikaans | 147.6 |
| 9 | Spanish, Portuguese, Galician | 147.5 |
| 10 | French, Catalan, Basque | 147.1 |
| 11 | Italian, Greek, Albanian | 108.0 |
| 12 | Turkish, Azerbaijani, Uyghur | 127.2 |
| 13 | Arabic, Hebrew, Persian | 107.1 |
| 14 | Hindi, Urdu, Bengali | 105.0 |
| 15 | Marathi, Gujarati, Malayalam | 104.0 |
| 16 | Tamil, Telugu, Kannada | 107.5 |
| 17 | Chinese (Simplified), Chinese (Traditional), Japanese | 236.1 |
| 18 | Korean, Vietnamese, Thai | 183.0 |
| 19 | Indonesian, Malay, Tagalog | 106.8 |

- The assignment above is the build's proposal under the maintainer's
  rule (by region, three each); 57 does not split evenly by region, so a
  few groups join neighbouring regions (11, 12, 18). The list lives in
  `tools/translation-models` and in the manifest; changing it is a
  rebuild of the files, nothing else.
- English is in every group (each language's two directions go through
  it), so there is no English download.
- A pair of two non-English languages in different groups needs both
  groups, and the server sees both.
- **What the request reveals:** the group (one of three languages, and the
  region by construction), the time and the IP address, to Lakko's server,
  and the size to anything on the path. No account; the server does not
  log it. The same class of disclosure ADR-0082 section 4 accepts for
  Apple's and Google's packs, with Lakko in the vendor's place.

The UX 3.5 dialog says it and claims no more (proposed copy):

> **Download languages for translation?**
> Translation happens on this device. To translate Spanish, Lakko needs
> the pack for Spanish, Portuguese and Galician (147.5 MB). It comes from
> Lakko's server, which learns that this device asked for one of these
> three languages. Messages are never sent anywhere to be translated.
> `Not now` · `Download`

- **Two groups.** A pair needing two names both and the summed size.
- **The size shown.** The transfer size from the manifest, never guessed
  (UX 3.5).
- **Anti-censorship.** In the desktop app with anti-censorship on, the
  request goes through the tunnel like every other request to Lakko's
  origin (ADR-0085 section 14). The amendment of 2026-11-19 stands: packs
  are offered whether it is on or off.
- **Section 3 for groups.** `/translation-models/<group file sha256>`; the
  manifest lists per group its file hash and size and, per direction, the
  offsets and SHA-256 of the model files inside it.
  - **As built:** the page fetches a group in ranges of 4 MiB (chosen): a
    response through the desktop app's anti-censorship tunnel may carry at
    most 8 MiB (`lakko-transport` `MAX_RESPONSE_BYTES`), and the desktop
    host hands the page a forwarded response whole, so ranges are also what
    moves the progress there. Every range must come back as a `206` with
    the exact `Content-Range`; a server that ignores ranges may answer the
    first request with the whole file.
  - The group file is kept only when its model files cover it exactly and
    each one's zstd bytes hash to the manifest, which checks every byte of
    it without hashing the whole file in one piece. The worker checks each
    model file's decompressed hash again before loading it.
  - The desktop host forwards `GET /translation-models/<64 hex>` to the
    Lakko origin like the release manifest, and logs no group name.
- **Exact sizes** (`tools/translation-models`, sums of Mozilla's files):
  the table above adds per-language sizes rounded to 0.1 MB; the exact
  group sizes differ from it by at most 0.11 MB (group 7: 144.7 MB). The
  dialog shows the manifest's exact size.

### 5. Storage on the device

- **Where.** Group files are stored as Blobs in an IndexedDB database of
  their own (`lakko-translation-models`), keyed by the group file's hash.
  (The first draft said the Cache Storage API; changed in the build: its
  keys must be http(s) URLs, and the desktop app's page lives on its own
  scheme.) They are per device, not per account: they are public files, and
  every account on the device reads the same languages from them. Translations themselves
  stay where ADR-0082 section 5 puts them, behind the ADR-0077 store gate.
- **What the person sees.** Settings, Language packs lists each Lakko pack
  with its size and `Remove`, as UX 6 already describes for packs Lakko
  downloaded. Forget device deletes the database, because a pack tells
  which languages the person reads.
- **Eviction.** The browser may evict the database under storage pressure.
  Lakko does not request `navigator.storage.persist()` (open question 3).
  An evicted pack reads `needs-pack` again, and the next Translate asks
  again (UX 3.5).
- **The interrupted line.** UX 3.5 already forbids a download line with
  nothing watching it; a download cut off by a closed tab leaves no partial
  file stored.

### 6. Place in the engine order

`device-engines.ts` appends a `lakkoTranslation()` engine after the
browser's:

| Client | Order for a pair |
|---|---|
| Chrome / Edge | browser `Translator`, then Lakko |
| Opera, Firefox, Safari, others | Lakko |
| Desktop app, macOS 26.4+ | Apple (host helper), browser (none in WKWebView), Lakko |
| Desktop app, Windows / Linux / older macOS | browser (WebView2 / WebKitGTK; whether WebView2 exposes `Translator` is not verified), Lakko |
| Web client in a phone browser | browser `Translator` where present, else none (open question 4) |
| iOS and Android apps | their system engines only (ADR-0082); this engine is not part of them |

The engine answers:

- **`state(source, target)`**
  - `unavailable`: no wasm SIMD, a pair the manifest lacks, or a worker that
    failed to start;
  - `ready`: every file of the pair (through English if needed) is in the
    cache and verified;
  - `needs-pack`: otherwise.
- **`packFetch()`** is `engine`, and **`fetchesPacks`** is true. The
  mobile-data switch of UX 6 then governs Lakko's downloads where the
  browser reports a connection type.
- **`translate()`** downloads after the person agreed (UX 3.5), reports
  progress, and is cancellable.

No screen and no controller logic change; that is what `engine-order.ts` was
written for.

### 7. Fail closed and tests

- **Nothing in the engine sends anything.** The engine has one network
  call, the model fetch from Lakko's origin under the manifest's paths.
  - A source test refuses any other `fetch`, `XMLHttpRequest`, `WebSocket`
    or `importScripts` URL in `lakko-engine/`, in the shape of the ADR-0082
    section 2 tests.
  - Message text crosses only into the worker via `postMessage`.
- **Tests:**
  - unit tests for `state()` over a fake cache, the manifest check, the
    pivot planner and the cancel path;
  - an engine test with the real wasm and the smallest model (the
    `lakko_segment.wasm` test pattern, skipped when the build is absent);
  - a firsthand walk in Firefox, Safari and Opera on test.lakko.me (S3);
  - the desktop app on this Mac with Apple's engines off for a pair.

## Alternatives

1. **Opus-MT (Helsinki-NLP Marian) through transformers.js and
   onnxruntime-web.**
   - What it has going for it: verified to run in a browser today;
     Apache-2.0 / MIT runtime; one model per pair, including non-English
     pairs.
   - Not chosen:
     - **Size.** Xenova/opus-mt-de-en at q8 is a 49.4 MB encoder plus a
       56.7 MB decoder, about 106 MB per direction (Hugging Face file
       listing), against Mozilla's 14 to 48 MB.
     - **Licence.** The weights' licence conflicts between sources: Apache-2.0
       on the Hugging Face card, CC-BY-4.0 in the Opus-MT README.
     - **Fetch.** onnxruntime-web fetches its wasm from jsDelivr unless told
       otherwise. That is fixable.
   - Kept as the fallback if Bergamot fails the S1 measurement.
2. **One bundle of every model, the ADR-0061 approach.**
   - The fetch would reveal no language.
   - Not possible at 2.53 GB compressed.
   - A middle form, downloading a fixed bundle of several languages
     together, still reveals the bundle. It is open question 2.
3. **Larger multilingual models.**
   - NLLB-200 distilled 600M: weights CC-BY-NC-4.0, and the card says
     "not released for production deployment". Excluded.
   - M2M-100 418M: MIT, but 1.94 GB of PyTorch weights.
   - MADLAD-400: Apache-2.0, smallest checkpoint 3B, 1.65 GB quantized.
   - None fits a browser download per language.
4. **A signed model manifest served apart from the build** (the ADR-0061
   artifact shape, a dedicated offline key).
   - It would allow model updates between web deploys.
   - Not needed while models change with deploys. It can be added without
     changing the engine.
5. **Fetching models from Mozilla's CDN directly.** It needs a CSP change.
   It hands every reader's IP address and language pair to a third party,
   and makes availability depend on Mozilla's Remote Settings layout.
   Rejected.
6. **A Rust engine in Lakko's own wasm crates** (candle's `marian-mt`
   example; burn; tract).
   - **No Rust runtime loads Mozilla's models.** Mozilla's student models are
     not the standard MarianMT transformer:
     - `student.base-memory.yml` in `mozilla/translations` uses
       `dec-cell: ssru` and an RNN autoregressive decoder;
     - candle's `marian.rs` implements only the Hugging Face MarianMT
       decoder.
   - Using Mozilla's models from Rust would mean writing all of these:
     - an SSRU decoder;
     - the lexical shortlist;
     - a converter from the fp32 `.npz` files in Mozilla's bucket;
     - int8 quantisation that keeps quality;
     - tuned wasm SIMD matrix multiplies.
   - **Opus-MT through candle runs.** Measured natively on this Mac (M3 Pro,
     2026-11-25, candle 0.11.1): opus-mt-en-ru fp32 translated one
     15-word sentence in 0.35 s warm. But the fp32 download is 292.7 MiB per
     direction, and candle has no quantised Marian.
   - **Measured in S1 (2026-11-25)**: the candle spike with opus-mt fp32
     in a Web Worker, same sentences and Mac as section 1. Median per
     sentence 164 to 295 ms, 47 to 61 words/s (about a tenth of Bergamot),
     wasm memory about 645 MiB. Safari did not finish the en->ru run within
     10 minutes; the cause is not investigated. Its Russian read more
     naturally in several sentences but lost the meaning of two (a birthday
     greeting turned into a different sentence). burn and tract are not
     measured.
   - Not chosen for v1: Mozilla's C++ engine already loads their models as
     shipped and builds for wasm. Kept as a later option, because the same
     crate could also serve iOS and Android.
7. **A small language model in the browser** (WebLLM, transformers.js,
   wllama).
   - **Size and hardware.** One download covers all languages, but it is 0.3
     to 2.7 GB:
     - TranslateGemma 4B at q4f16 is about 2.7 GB;
     - WebLLM lists 0.7 to 2 GB of GPU memory for 1-2B models.
     Usable speed needs WebGPU, which MDN lists as absent on Firefox for
     Linux, Intel Macs and Android, and WebKitGTK has not enabled.
   - **Licences.** The small models with good translation scores all carry
     terms that restrict use:
     - Gemma Terms of Use: §3.2 lets Google restrict usage remotely;
     - the Tencent HY licence excludes the EU, the UK and South Korea;
     - Tower, Aya and NLLB are CC-BY-NC.
     The Apache-2.0 small models publish no translation score; on
     FLORES+ Qwen3-0.6B scores COMET 54.86 (arXiv 2602.11961).
   - **Rejected for a messenger: the message text is the attacker's input.**
     A language model can be steered by instructions inside the text it
     translates (arXiv 2403.09832, 2410.05047). A reader would then be shown
     text the sender chose, presented as the meaning of the message.

## Consequences

- **Positive:**
  - Translation in every browser with wasm SIMD, and in the desktop app on
    Windows, Linux and older macOS.
  - No message text leaves the device.
  - No new service; the only server change is static files and one nginx
    location.
  - Engines and screens keep the shape ADR-0082 gave them.
- **Negative:**
  - Lakko's server learns, per download, which language pair a device asked
    for. The dialog says so.
  - The first translation of a language costs a 14 to 48 MB download, or two
    of them for a pair through English.
  - The wasm fallback gemm is slower than Firefox's own path (the cited bug
    figures).
  - Pairs through English translate twice, and quality drops accordingly.
  - The mirror is 2.53 GB of static files on the VPS (all 114 directions,
    compressed).
  - Lakko takes on an emsdk build and a vendored MPL-2.0 C++ dependency.
- **Neutral:**
  - Model updates follow Mozilla's releases through `tools/translation-models`
    and a web deploy.
  - iOS and Android are unchanged: they use their system engines and never
    this one (open question 4).

Invariants:

- #2 untouched: the server receives no message data; the language-pair
  disclosure is metadata about a download, stated above.
- #4: the section 2 local-only rule of ADR-0082 holds by construction and
  is tested.
- #5 untouched: no account on the request.
- `lakko-mls` untouched; not a crypto change. A `lakko-security` pass is
  optional; it would look at section 3 (integrity in the desktop app) and
  section 4.

## Build plan (after acceptance)

- **S1 - spike and measurement.** Build `lakko_translate.wasm` from the
  pinned commit. Load es->en, en->ru and a `tiny` pair in a worker. Measure
  wasm size, load time, words per second and peak memory in Firefox, Safari
  and Opera on this Mac. Report the numbers to the maintainer before S2.
- **S2 - engine.** `lakko-engine/` worker and glue; `tools/translation-models`
  and the manifest; the cache; `device-engines.ts`; the unit and engine
  tests; REUSE annotations.
- **S3 - serving and walk.** nginx location on the test stack, the files
  mirrored, the dialog copy (UX 3.5 amendment, catalog keys in the 11
  languages). Walk in Firefox, Safari and Opera on test.lakko.me, and in the
  desktop app.

## Open questions (the maintainer's)

1. **v1 language set.** Decided 2026-11-25 (maintainer): all 114 desktop
   directions Mozilla ships without a channel filter.
2. **Language privacy of the download.** Decided 2026-11-25 (maintainer):
   groups of three languages, by region; one file per group (section 4).
3. **Persistent storage.** Decided 2026-11-25 (maintainer): Lakko does not
   request `navigator.storage.persist()`. An evicted group reads
   `needs-pack` and the next Translate shows the dialog again.
4. **Phones.** Decided 2026-11-25 (maintainer): wherever a native engine
   exists it is used, and this engine only where there is none. The iOS and
   Android apps translate with their system engines (ADR-0082) and never
   with this one. A phone has a native engine through the Lakko app, so the
   web client opened in a phone browser does not offer this engine either;
   it keeps the browser's `Translator` where the browser has one. Tablets
   are treated the same (the iPad and Android apps have the system
   engines). The web client tells them by `navigator.userAgentData.mobile`,
   else the user agent string, and an iPad that reports a Mac by its touch
   points (`lakko-engine/device.ts`). Nothing is measured on phones.
5. **Training-data licences.** Decided 2026-11-25 (maintainer): ship on
   Mozilla's MPL-2.0 statement for the model files. The licences of the
   training corpora are not stated by Mozilla and are not known to Lakko.
6. **Compression on the wire.** Decided in the build (not a product
   decision): the group file carries Mozilla's zstd files byte for byte, so
   each one's hash is Mozilla's, and the worker decompresses them with the
   zstd decoder compiled into `lakko_translate.wasm`. Measured for the
   es->en model file: brotli 22.5 MB against 22.9 MB for zstd -19, so
   recompressing would save little and would replace Mozilla's hashes
   with Lakko's own.
7. **Transcription.** On-device speech recognition for the same browsers
   (for example whisper.cpp compiled to wasm) is a separate ADR, not part of
   this one.

## References

- ADR-0082 (note of 2026-11-18, amendment of 2026-11-19), ADR-0061
  section 5, ADR-0047 (access-log precedent), ADR-0077, ADR-0085
  section 14.
- `mozilla/translations` README and `inference/`;
  https://github.com/mozilla/translations
- Firefox source docs, Translations overview and Bergamot:
  https://firefox-source-docs.mozilla.org/toolkit/components/translations/resources/01_overview.html
- Remote Settings collections `translations-models-v2` and
  `translations-wasm-v2`:
  https://firefox.settings.services.mozilla.com/v1/buckets/main/collections/translations-models-v2/changeset?_expected=0
- Mozilla Public License 2.0, sections 1.12 and 3.3:
  https://www.mozilla.org/MPL/2.0/
- Bugzilla 1746631 and 1926100 (wasm gemm fallback speed).
- mozilla/standards-positions#1015; WebKit/standards-positions#339.
- MDN browser-compat-data 8.1.10 (`Translator`, `LanguageDetector`,
  WebAssembly fixed-width SIMD).
- Hugging Face: Xenova/opus-mt-de-en (ONNX file sizes),
  facebook/nllb-200-distilled-600M, facebook/m2m100_418M,
  google/madlad400-3b-mt.
