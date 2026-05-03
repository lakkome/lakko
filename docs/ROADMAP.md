<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# Lakko — Roadmap

Полная декомпозиция фаз — что строится и в каком порядке. Стратегия живёт в `~/.claude/plans/1-nested-wreath.md`; ADR в `docs/adr/` — авторитативный источник архитектурных решений; этот документ — **тактика разработки фич**.

---

## Контекст исполнения

**Этот roadmap не календарный.** Ни один deliverable не привязан к неделям / месяцам / годам.

- Код пишет AI-агент (Claude Code) в рамках сессий, инициированных maintainer-ом
- **Sprint** = atomic deliverable, который умещается в 1-3 когерентных сессии и заканчивается зелёным CI
- Темп фаз определяется тем, когда maintainer садится работать
- **Phase N+1 не стартует** пока exit criteria Phase N не зелёные — это единственная зависимость
- Stack: Rust core + native UI per platform; MariaDB; Hetzner Cloud как hosting

Решения про launch / audit / branding / pricing — за maintainer-ом. Этот roadmap занимается **только тем что и в каком порядке писать**.

---

## Сводка фаз

| Фаза | Что добавляется |
|---|---|
| **0** Foundations | Workspace, ADR, CI, MLS end-to-end в тестах, FFI surface, fuzz harnesses |
| **1** 1×1 Messaging | services/auth, services/ds, services/storage, services/discovery, services/push-gw, lakko-ffi-wasm, apps/ios, apps/android, apps/web |
| **2** Groups + Multi-device | X.509 + KT log, DeviceGroup, encrypted backup, ≤50-членные группы, apps/desktop, apps/macos |
| **3** Calls | lakko-calls (SFrame), services/sfu (self-hosted LiveKit), P2P + group calls (link и in-chat) + audio rooms, CallKit / ConnectionService, coturn |
| **4** Channels / Broadcasts | PublisherGroup, sender-key broadcast, services/edge, medium-scale broadcasts |
| **5** Anti-censorship + Scale | REALITY, MASQUE, Shadowsocks-2022, transport-rendezvous, multi-region edge cascade, apps/watchos, localization |

---

## Cross-cutting

### Code quality gates

| Gate | С какой фазы | Тип |
|---|---|---|
| `cargo fmt --check` | 0 | blocking PR |
| `cargo clippy -- -D warnings` | 0 | blocking PR |
| `cargo test --workspace` | 0 | blocking PR |
| `cargo doc -D warnings` | 0 | blocking PR |
| `cargo deny check` | 0 | blocking PR |
| `reuse lint` | 0 | blocking PR |
| `cargo audit` | 0 | blocking PR (vulnerability) |
| `cargo fuzz` парсеров | 0.4 | continuous (OSS-Fuzz integration) |
| `kani` model checking MLS state machine | 2 | nightly |
| Coverage ≥80% (crypto + mls) | 2 | blocking PR |
| Mutation testing crypto (`cargo-mutants`) | 2 | nightly |
| Hax / F* formal verification critical paths | 4+ | aspirational |
| SLSA L3 build provenance | 1 | release |
| Reproducible build verifier (dual runner) | 0.4 | release |

### Hosting / infra

Всё self-hosted на Hetzner Cloud. Стартуем с одного VPS, разносим когда упёрлись в потолок (CPU / RAM / bandwidth ceiling). Object storage — Hetzner Storage Box или local disk.

| Фаза | Что нужно технически |
|---|---|
| 0 | — (нет prod) |
| 1 | 1× VPS со всеми services + MariaDB + nginx + coturn co-located |
| 2 | scale-up VPS или 2× VPS (backend + DB) когда упрёмся |
| 3 | + 1× CCX-tier VPS под SFU (LiveKit / mediasoup-rs / pion / str0m), 20 TB transfer |
| 4 | scale-up SFU при росте broadcast viewer load |
| 5 | multi-region edges (для 100k+ viewer broadcasts с low-latency глобально) |

---

## Phase 0 — Foundations

### Цель

Из пустого репо к workspace, где MLS-группы работают end-to-end в тестах, и FFI-surface стабилен достаточно для генерации Swift / Kotlin биндингов.

### Pre-flight

— (стартовая фаза).

### Sprint breakdown

| Sprint | Содержимое | Статус |
|---|---|---|
| **0.0** | Cargo workspace + ADR-0001…0006 + CI workflow + Nix flake + justfile + REUSE compliance | ✅ done |
| **0.1** | CI gates на GitHub Actions + lakko-proto codegen (protox + prost-build) + lakko-crypto primitives | ✅ done |
| **0.2** | lakko-mls поверх OpenMLS (identity / key_package / group / exporter) + end-to-end Alice ↔ Bob ↔ Charlie | ✅ done |
| **0.3** | lakko-transport (QUIC + in-memory) + lakko-ffi-uniffi (Identity / Group surface) | ✅ done |
| **0.4** | Fuzz harnesses (cargo-fuzz) на парсеры (MLS wire, prost); reproducible build verifier (dual runner в CI). OSS-Fuzz upstream PR — при первой реальной интеграции | ✅ done |
| **0.5** | KT log skeleton — `lakko-transparency` crate (Merkle RFC 6962 + BLAKE3, SignedRoot Ed25519, InclusionProof, InMemoryLog, rollback detection). X.509 credentials + FIPS suite unblock — отложено в Phase 1 (нет crate `openmls_x509_credential` в реестре, требуется upstream contribution или manual TLS-codec encoding) | ✅ done (KT) / deferred (X.509+FIPS) |

### Exit criteria
- ✅ MLS groups end-to-end в integration тестах
- ✅ FFI surface стабилен (lakko-ffi-uniffi компилируется, three-party FFI test зелёный)
- ✅ CI gates все зелёные
- ✅ Fuzz harnesses на каждый wire-format парсер (MLS message, KeyPackage, prost wire/mls_app/media)
- ✅ Reproducible build verifier — CI job есть (continue-on-error пока тюним flags для byte-identicality)
- ✅ KT log skeleton — `lakko-transparency` crate (production storage в Phase 2)
- 🟡 X.509 credentials + FIPS — deferred к Phase 1; нужны либо upstream `openmls_x509_credential` crate, либо manual TLS-codec encoding cert chain

### Top risks
- **OpenMLS upstream churn** — митигировано: fast-follow upgrades (мы уже сделали 0.6 → 0.8 при появлении hpke-rs CVE)
- **uniffi 0.28 stale deps** (`paste`, `bincode`) — отмечены в `deny.toml` как ignore с привязкой к upstream

---

## Phase 1 — 1×1 Messaging

### Цель

Первый реальный функциональный срез: три устройства (iOS, Android, web) обмениваются 1×1-сообщениями через настоящий backend, self-hosted на одном Hetzner VPS. Web-клиент — параллельный track к нативным mobile, чтобы dogfooding не упирался в Apple/Google аккаунты и review-пайплайны.

### Pre-flight
- Phase 0 exit criteria зелёные (включая 0.4 fuzz и 0.5 X.509 + KT skeleton)

### Sprint breakdown

#### Backend infra
- **1.0** Hetzner deployment skeleton (✅ done): миграция от lakko_me Node.js stack → новый Rust stack на ТОЙ ЖЕ VPS. systemd unit-ы для 5 services (auth/ds/storage/discovery/push-gw) + group target; system-nginx config с reverse-proxy на 127.0.0.1:808X для существующих доменов lakko.me / api.lakko.me; system-MariaDB (drop старой `lakko_me` БД, новая `lakko`); `/etc/lakko/lakko.env` для shared config; `infra/scripts/deploy.sh` (git pull → cargo build --release → install → systemctl restart); пошаговый migration guide в `infra/README.md`. Контейнеризация (podman/compose) намеренно отложена — overkill для solo + 1 VPS, добавим когда понадобится horizontal scaling
- **1.1** `services/auth` — OPAQUE-PAKE password auth (primary), MariaDB-backed signup/login без хранения PII; BIP39 mnemonic recovery; PoW (Equix) + Privacy Pass (RFC 9576) anti-spam. WebAuthn passkey **отложен в Phase 2+** как opt-in second factor — passkey UX в 2026 ещё проблематичен для mass-market (cross-ecosystem sync issues, recovery friction, mental-model confusion); password-flow закрывает 100% юзеров без friction. Sub-sprint-ы: 1.1a (✅ axum + sqlx + probes), 1.1b (✅ OPAQUE registration), 1.1c (OPAQUE login), 1.1d (BIP39 recovery), 1.1e (PoW + Privacy Pass)
- **1.2** `services/storage` — простой ciphertext blob proxy на Hetzner Storage Box (или local disk + nginx); content-addressable, ciphertext-only policy
- **1.3** `services/ds` skeleton — MariaDB-backed append-only журнал per-group (FIFO по `(epoch, seq)`), mailbox per device, axum + tokio
- **1.4** `services/ds` production — fanout, long-poll или QUIC stream push для real-time delivery
- **1.5** `services/discovery` — username table + OPRF (RFC 9497) blinded lookup, exact-match only, PoW per-query, MariaDB-backed
- **1.6** `services/push-gw` — APNs + FCM + Web Push (VAPID) envelope routing, payload только `(group_pseudonym, envelope_pointer)`; UnifiedPush self-host (на том же VPS, для CN/RU prep)

#### Crates (расширение)
- **1.7** `lakko-storage` — SQLCipher-backed local store (native), IndexedDB-backed для web; ключ из HKDF от device root key (Secure Enclave / StrongBox / non-extractable WebCrypto)
- **1.8** `lakko-transport` production wiring — QUIC client (native) + WebTransport-fallback / WebSocket (web) через `services/ds`, cert pinning skeleton
- **1.9** `lakko-contacts` — ContactRequest end-to-end, capability-based invite links (см. ADR-0005); pseudonym lifecycle minimum
- **1.10** `lakko-push` — encrypted push payload framing (APNs / FCM / Web Push форматы)
- **1.11** `lakko-presence` — blinded per-conversation presence tokens (минимальная версия)
- **1.12** `lakko-sync` — hybrid logical clock + draft sync (single-device variant)
- **1.13** `lakko-ffi-wasm` — wasm-bindgen surface зеркало UniFFI (Identity / Group / KeyPackage / процессинг сообщений)

#### Native apps (iOS + Android)
- **1.14** `apps/ios` skeleton — SwiftUI: signup/signin (OPAQUE-PAKE), contact list UI, 1×1 chat UI; integration с XCFramework из lakko-ffi-uniffi
- **1.15** `apps/android` skeleton — Compose: тот же surface; AAR-integration

#### Web app
- **1.16** `apps/web` skeleton — Solid.js + WASM core (через lakko-ffi-wasm); signup/signin (OPAQUE-PAKE через WebCrypto-derived OPRF), contact list UI, 1×1 chat UI; service worker для offline; bundle target <2 MB gzipped
- **1.17** `apps/web` static-host pipeline — GitHub Pages или Hetzner static behind nginx; SRI hashes для security; build artefacts reproducible

#### Cross-platform features
- **1.18** Apps: ContactRequest end-to-end через DS, invite links flow (iOS + Android + Web)
- **1.19** Apps: 1×1 chat — encrypted text + attachments через storage service
- **1.20** Apps: push notifications — APNs (iOS) / FCM (Android) / Web Push VAPID (browsers); encrypted envelopes на всех путях
- **1.21** Apps: recovery flow — BIP39 24-word mnemonic, optional Shamir 3-of-5 social recovery (если хватит сил в этой фазе)

#### Готовность
- **1.22** Staging deployment — один Hetzner VPS, nginx + MariaDB + все services co-located, Let's Encrypt; web-клиент развёрнут на том же домене или на статике

### Exit criteria
- 1×1 messaging работает end-to-end на реальных iOS + Android устройствах **и в браузере** через Hetzner backend
- Cross-platform взаимодействие: web-клиент и mobile decrypt-ят сообщения друг от друга идентично
- Push delivery p50 < 5s (на всех трёх каналах: APNs / FCM / Web Push)
- Message send → delivered p50 < 1s regional
- DS выдерживает 1k одновременных connection-ов в local load test
- Recovery: можно потерять устройство и восстановить аккаунт по mnemonic
- Reproducible build artifacts проверены (XCFramework + AAR + WASM bundle byte-identical между runner-ами)
- Web bundle < 2 MB gzipped

### Top risks
- **iOS background limitations** — APNs delivery надёжность; MLS state machine recovery после долгой spavnki
- **OPAQUE production maturity** — RFC ещё draft; track upstream
- **Hardware-backed key APIs differ** между iOS / Android / WebCrypto — разные failure modes
- **WASM bundle size** для web — OpenMLS + RustCrypto могут давать heavy bundle; готовность к code splitting и lazy-loading нечастых code paths
- **Web Push VAPID setup** — нужны VAPID ключи + сервер-сайд push-вызовы через `services/push-gw`

---

## Phase 2 — Groups + Multi-device + Desktop

### Цель

Группы до ~50 человек (vanilla MLS), multi-device sync без plaintext на сервере, добавляются desktop (Windows / Linux) и macOS клиенты. iOS / Android / Web уже шипятся в Phase 1.

### Pre-flight
- Phase 1 exit criteria зелёные

### Sprint breakdown

#### Crypto / protocol
- **2.0** X.509 credentials production-grade (full validation) + KT log production (CONIKS-style append-only с signed roots)
- **2.1** Identity rotation через KT log supersession proof + 30-day bridge для старых invite links
- **2.2** FIPS ciphersuite (P-384 + AES-256-GCM + ECDSA-P384) wired через X.509 credentials end-to-end

#### Multi-device
- **2.3** DeviceGroup в lakko-mls + lakko-sync — каждое устройство = leaf в личной MLS-группе пользователя
- **2.4** Multi-device sync substrate — read receipts, drafts, conversation order через DeviceGroup application messages
- **2.5** History catch-up bundle exchange — HPKE-encrypted ciphertext от старого устройства новому при добавлении
- **2.6** Encrypted backup в Hetzner Storage Box, KEK = Argon2id от recovery passphrase
- **2.7** Optional Shamir 3-of-5 social recovery (если не закрыто в Phase 1)

#### Группы
- **2.8** Group creation с ≥3 членами (vanilla MLS до ~50)
- **2.9** Group invite links (capability-based, ADR-0005) + approval flow для приватных групп
- **2.10** Group settings: encrypted name / avatar / description

#### Discovery / privacy
- **2.11** Все 6 уровней discoverability scope: Open / ContactsOfContacts / Contacts / InviteOnly / Allowlist / VerifiedNetwork
- **2.12** Mutual-contact attestation chain для ContactsOfContacts scope
- **2.13** Block list (encrypted под identity, server применяет hashed match)

#### Apps (новые)
- **2.14** `apps/desktop` — Tauri 2.x + Solid.js (UI re-use из apps/web), Windows / Linux native binaries
- **2.15** `apps/macos` — отдельная сборка через SwiftUI XCFramework (общий core с iOS)
- **2.16** Расширение apps/web под групповой UX (member-list, group settings, etc.)

#### Verification & QR
- **2.17** QR safety numbers — Signal-style fingerprint scan, добавление контакта OOB-secure
- **2.18** TOFU pinning через KT log + QR override

#### Hardening
- **2.19** `kani` model checking на MLS state machine
- **2.20** `cargo-mutants` mutation testing на lakko-crypto + lakko-mls
- **2.21** Coverage ≥80% на lakko-crypto + lakko-mls
- **2.22** End-to-end тесты на iOS / Android / Web в CI (matrix)

### Exit criteria
- Feature-complete messaging на 5 платформах (iOS / Android / macOS / Linux / Web)
- Группа на 50 членов стабильна в load test (Add/Remove под нагрузкой, 100+ commit-ов в час)
- Multi-device sync convergent в p95 < 5s между двумя устройствами
- ≥80% statement coverage на lakko-crypto + lakko-mls
- WASM bundle <2 MB gzipped
- KT log validates inclusion proofs end-to-end

### Top risks
- **KT log spec ещё draft** — возможен fork / patch upstream
- **WASM bundle size** для web (target <2 MB gzipped) — может потребовать code splitting
- **Cross-device sync convergence** под flaky networks
- **OpenMLS X.509 credential maturity** — может потребовать upstream contributions

---

## Phase 3 — Calls

### Цель

P2P 1×1 + group audio/video звонки + audio rooms. Self-hosted SFU (LiveKit upstream без форка для начала, либо mediasoup-rs / pion / str0m) на одном Hetzner VPS. Media-keys из MLS exporter, SFrame для frame-level encryption.

Multi-region edge cascade и broadcasts на 100k+ viewers — Phase 5 (там действительно нужны geographically distributed edges).

### Pre-flight
- Phase 2 exit criteria зелёные

### Sprint breakdown

#### Backend
- **3.0** coturn self-hosted на том же Hetzner VPS — STUN + TURN-over-TLS
- **3.1** Сигналинг для call setup через `services/ds` (специальный тип MLS-application-message в группе звонка)
- **3.2** `services/sfu` — self-hosted LiveKit (Apache 2.0 upstream) или альтернатива (mediasoup-rs / pion / str0m); JWT-auth интеграция с `services/auth`; отдельный VPS

#### Crates
- **3.3** `lakko-calls` skeleton — SFrame (RFC 9605) implementation, jitter buffer (NetEQ-style), codec negotiation
- **3.4** `lakko-calls` ICE/STUN/TURN client integration (через webrtc-rs или str0m)
- **3.5** `lakko-mls::group` exporter API расширение для call media keys (`mls_exporter("lakko/call/v1", call_id, 32)`)

#### Топологии
- **3.6** P2P 1×1 setup: WebRTC + ICE/STUN/TURN, никакого SFU; media keys из 1×1 MLS group exporter
- **3.7** Group call по линку: SFU + ad-hoc MLS group, join по embedded one-time KeyPackage
- **3.8** Group call внутри чата: SFU + reuse существующей chat MLS group; exporter context = `call_id`
- **3.9** Audio room: Speakers MLS subgroup (vanilla, ≤50 speakers) + Audience через SFU subscribe-only
- **3.10** Encrypted media: SFrame keys из exporter, AES-128-GCM на frame, hardware AES-NI / ARM Crypto Extensions; key rotation на Add/Remove с 1s overlap

#### Кодеки
- **3.11** Audio: Opus 32-64 kbps + DTX + FEC
- **3.12** Video: AV1-SVC (modern), VP9-SVC (fallback), H.264 (Apple legacy)
- **3.13** Adaptive bitrate by receiver BWE feedback (через LiveKit/SFU-side logic)

#### Apple integration
- **3.14** CallKit для нативного UI входящих
- **3.15** PushKit для wake-up на incoming call
- **3.16** Live Activities для in-progress call indicator

#### Android integration
- **3.17** ConnectionService для system-managed incoming UI
- **3.18** MediaSession для controls
- **3.19** ForegroundService для long-running call

#### Доп
- **3.20** Screen sharing — один screen producer, pinned layout
- **3.21** In-call chat — обычные MLS application messages в группе звонка (existing flow)
- **3.22** Promotion / demotion в audio room: Add/Remove на Speakers MLS subgroup + key rotation

### Exit criteria
- P2P 1×1 audio + video call работает end-to-end на iOS + Android + Web + Desktop
- Group call (link и in-chat) — до 30 участников через single self-hosted SFU
- Audio room — до 30 speakers + ≤500 listeners через single SFU
- Audio quality A/B протестировано против Telegram / WhatsApp / Discord в lab; не хуже p50
- Audio p50 latency < 100 ms regional (P2P) / < 150 ms (через SFU)
- Screen sharing работает на macOS / Linux / Windows
- E2EE strict — никакого silent fallback (включая медиа в group call)
- CallKit / ConnectionService integration корректные (incoming UI работает в background)
- SFU выдерживает 5 одновременных group calls по 10 участников в local load test

### Что НЕ в скоупе Phase 3
- Каналы / sender-key broadcast (Phase 4)
- 100k+ viewer broadcasts (Phase 5 — нужен multi-region cascade)
- Recording (отдельный отложенный вопрос — policy-clarity)

### Top risks
- **iOS background CPU limits** во время длинных звонков
- **AV1 hardware decode** на старых устройствах (fallback на H.264)
- **TURN abuse** — self-hosted coturn нужен rate-limit + auth
- **CallKit / ConnectionService** API differences create UX inconsistency между iOS и Android
- **SFU bandwidth ceiling** — single VPS на CCX-tier даёт ~60 Mbps sustained; ~30 одновременных 4-человек видео-call-ов; scale-up при необходимости

---

## Phase 4 — Channels + Medium Broadcasts

### Цель

Channels (sender-key broadcast через DS) и medium-scale broadcasts (≤10k одновременных viewers через single SFU).

Audio rooms уже сделаны в Phase 3. Multi-region edge cascade для 100k-1M viewers — Phase 5 (там действительно нужны geographically distributed edges и multi-CDN).

### Pre-flight
- Phase 3 exit criteria зелёные

### Sprint breakdown

#### Channels (sender-key broadcast)
Vanilla MLS не масштабируется к 1M — реализуем **two-tier** архитектуру (см. план §3 / ADR-0001):

- **4.0** PublisherGroup в lakko-mls — vanilla MLS, ≤500 mods/admins, content-key derivation API
- **4.1** Sender-key envelope distribution через DS — HPKE-to-KeyPackage envelope для каждого нового подписчика
- **4.2** Content-key rotation — на schedule (24h / N сообщений) + на membership change PublisherGroup
- **4.3** Channel subscribe / unsubscribe flow + best-effort revoke
- **4.4** Reactions + comments на channel posts (per-post small MLS group, дин. создаётся)
- **4.5** Public channel discovery (`@channel-username`) через `services/discovery` extension

#### Medium broadcasts (single-region SFU)
- **4.6** `services/edge` — broadcast origin handler; reuse-ит SFU из Phase 3 для media-pipe
- **4.7** Late-joiner flow для broadcasts — viewer получает current content key через HPKE-envelope
- **4.8** Reactions от viewers (через тот же sender-key broadcast back-channel)

#### Public posture
- **4.9** Документировать публично: backward-secrecy property каналов (то же что Telegram/Slack/Discord; мы делаем ротацию агрессивной)

#### Load testing
- **4.10** 50k-subscriber channel stable (rotation event + delivery)
- **4.11** 10k-viewer broadcast end-to-end через single SFU

### Exit criteria
- Channel до 50k подписчиков стабилен (rotation events не валят DS)
- Broadcast до 10k viewers через single SFU стабилен (scale-up VPS если упрёмся)
- Public channels находятся через `@username`
- Documented publicly: backward-secrecy свойство каналов

### Что НЕ в скоупе Phase 4
- 100k-1M viewer broadcasts (Phase 5 — нужен multi-region edge cascade)
- Channel federation (вне roadmap)
- Recording channels / broadcasts (отдельный policy-вопрос, не функциональный)

### Top risks
- **Sender-key distribution scaling** — большой канал = много HPKE-envelope-ов на ротации; DS bandwidth budget; митигируется staggered rollout
- **Revocation после раздачи content key** — best-effort до ротации; community reaction на этот факт
- **DS load** при rotation event для крупных каналов (нужен throttling)
- **Single-SFU broadcast ceiling** — после ~10k viewers начинается degradation, дальше нужен cascade (Phase 5)

---

## Phase 5 — Anti-censorship + Scale

### Цель

Anti-censorship transport stack (для пользователей в RU / IR / CN) + multi-region edge cascade для broadcasts на 100k+ одновременных viewers + watchOS + локализация.

### Pre-flight
- Phase 4 exit criteria зелёные

### Sprint breakdown

#### Pluggable transports (см. ADR-0004)
- **5.0** REALITY (xtls/REALITY) в lakko-transport — TLS-in-TLS под чужой сертификат
- **5.1** MASQUE / CONNECT-UDP (RFC 9298) — туннель QUIC внутри HTTP/3
- **5.2** Shadowsocks-2022 (AEAD-2022)
- **5.3** VLESS + XTLS-Vision
- **5.4** WebTransport для web-клиента
- **5.5** obfs4 — legacy compatibility

#### Bootstrap rendezvous
- **5.6** ECH (RFC 9460/9525) на edge endpoint-ах
- **5.7** `services/transport-rendezvous` — производит signed bootstrap configs (Ed25519, JSON: endpoints + working PT configs)
- **5.8** DoH-resolved (Cloudflare 1.1.1.1, Google, Quad9, Mullvad) ротирующийся пул innocuous-looking доменов
- **5.9** TUF (The Update Framework) signed transport configs — новые транспорты как **данные**, не код
- **5.10** Auto-discovery: happy-eyeballs racing, top-2 рабочих держим warm, бюджет ≤10 s cold start

#### Mobile transport integration
- **5.11** iOS: pluggable transports встраиваем в URLSession+QUIC (не через NEPacketTunnelProvider)
- **5.12** Android: VpnService доступен опционально, default — in-app

#### Multi-region broadcast cascade (для broadcasts на 100k+ viewers)
- **5.13** `services/edge` extension — origin SFU → edge SFU cascade с region-affinity routing
- **5.14** Edge nodes deployment в нескольких Hetzner регионах (Helsinki / Falkenstein / Nuremberg / Ashburn) или multi-CDN
- **5.15** Late-joiner flow для cascade broadcasts
- **5.16** Load testing: 100k+ concurrent viewers распределённых по регионам

#### watchOS
- **5.17** `apps/watchos` — thin presenter, paired secure channel с phone

#### Localization
- **5.18** Translation pipeline (Crowdin или self-hosted) для всех клиентов
- **5.19** 10+ языков (ru / en / fa / zh-CN / ar / es / pt / de / fr / ja / ko / uk / …)
- **5.20** RTL layouts

#### Polish
- **5.21** Onboarding flow (минимум friction, secure-by-default)
- **5.22** Accessibility (VoiceOver / TalkBack полная поддержка)
- **5.23** Анимации, переходы

### Exit criteria
- Успешные подключения из ≥5 censorship-emulating environments (GFW / TSPU / IR DPI / corporate firewall) в lab
- Broadcast 100k+ concurrent viewers стабилен через cascade
- WatchOS app работает
- Localization для 10+ языков с native review
- Auto-discovery транспортов выбирает рабочий в ≤10 s cold start

### Top risks
- **Censorship arms race** — нужны быстрые TUF-signed transport rollouts (новые транспорты как data, не code)
- **iOS Network Extension limitations** ограничивают anti-censorship — fallback на in-app transports
- **Hetzner multi-region** — все hetzner DC в EU; для US / APAC users придётся либо терпеть latency, либо подключать другие провайдеры
- **DPRK-style allowlist-only internet** — сервис там недоступен; **abandon plan**: не инженерим

---

## Post-Phase-5 (вне скоупа этого roadmap)

После Phase 5 normal product evolution. Технические направления, которые имеет смысл отслеживать:

- **Federation** между Lakko-инстансами
- **Post-quantum** — MLS extensions для ML-KEM в работе IETF; мигрируем когда зрелое и доступно в OpenMLS
- **libcrux replacement** — переход на формально верифицированный crypto provider
- **OpenMLS upstream contributions** — резерв capacity на upstream
- **SDK split** — выделить `lakko-mls` / `lakko-crypto` / `lakko-proto` под Apache 2.0 в отдельный sub-repo если индустрия востребует embedded MLS implementation; благодаря CLA это возможно

---

## Что важно помнить

- **Roadmap не календарный** — sprint = atomic deliverable, не временной слот
- **Phase N+1 не стартует** пока exit criteria Phase N не зелёные — единственное жёсткое правило
- **Документ обновляется** — после закрытия каждой фазы добавляем `### Retrospective` секцию с тем что прошло как ожидалось vs что нет
