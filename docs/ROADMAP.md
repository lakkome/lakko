<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# Lakko — Roadmap по фазам

Полная декомпозиция шести фаз от пустого репо до публичного запуска (~36 месяцев). Кратко-табличная сводка живёт в `~/.claude/plans/1-nested-wreath.md` §9; этот документ — расширенная, исполнительная версия.

ADR в `docs/adr/` — авторитативный источник архитектурных решений. План — стратегия. Этот файл — **тактика**.

---

## Сводка

| Фаза | Месяцы | Цель | Внешний аудит |
|---|---|---|---|
| **0** Foundations | 0–4 | Workspace, ADR, CI, MLS end-to-end в тестах, FFI surface | — |
| **1** 1×1 Alpha | 4–9 | Реальный 1×1 messaging на iOS + Android | — |
| **2** Groups + Multi-device | 9–15 | Группы до 1k, sync, desktop + web | **Перед public beta** |
| **3** Calls | 15–22 | P2P, group calls (link и in-chat) с E2EE медиа | SFrame integration |
| **4** Channels + Broadcasts + Rooms | 22–30 | Telegram-parity content surfaces | Scaling crypto |
| **5** Anti-censorship + Launch | 30–36 | RU/IR/CN-устойчивость + публичный релиз | **Final pre-launch** |

Размер команды растёт ~5 → ~25 от Phase 0 к Phase 5; Phase 0 — критический путь, не параллелится.

---

## Cross-cutting (через все фазы)

### Внешние аудиты
- **Phase 0 → 2:** только внутреннее ревью; OpenMLS upstream tracking
- **Phase 2 exit:** crypto + MLS state machine + auth + KT log — обязателен перед public beta
- **Phase 3 exit:** SFrame integration с MLS exporter
- **Phase 4 exit:** scaling crypto (sender-keys, content-key rotation, revocation семантика)
- **Phase 5 exit:** **финальный pre-launch** — full surface, findings публикуются после remediation
- **Post-launch:** ежегодный third-party + bug bounty (HackerOne)

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
| `cargo fuzz` парсеров | 1 | continuous |
| `kani` model checking MLS | 2 | nightly |
| Coverage ≥80% (crypto+mls) | 2 | blocking PR |
| Mutation testing crypto | 2 | nightly |
| Hax / F* formal verification | 4 | aspirational |
| SLSA L3 build provenance | 2 | release |
| Reproducible build verifier | 1 | release |

### Инфраструктура — масштабирование
- **Phase 0:** локальные тесты, нет deployed-сервисов
- **Phase 1:** single-region staging (Hetzner / Hetzner Cloud), Let's Encrypt
- **Phase 2:** multi-region (US East + EU West), DS shards, FoundationDB cluster
- **Phase 3:** региональные edge SFU
- **Phase 4:** глобальный edge cascade для broadcasts
- **Phase 5:** multi-CDN bootstrap, edge в censorship-регионах

### Команда (ориентировочно)
- **Phase 0:** 5 (Rust core + infra)
- **Phase 1:** 8–10 (+ iOS / Android / backend leads)
- **Phase 2:** 15 (+ web / desktop / больше backend)
- **Phase 3:** 18–20 (+ WebRTC engineer)
- **Phase 4:** 22–25 (+ scaling engineers)
- **Phase 5:** 25 + comms / legal / security ops

---

## Phase 0 — Foundations (months 0–4)

### Цель

Из пустого репо к workspace, где MLS-группы работают end-to-end в тестах и FFI-surface стабилен достаточно для генерации Swift / Kotlin биндингов.

### Статус

Sprint 0.3 закрыт (transport + FFI). Phase 0 фактически на финише — остаются только подчищающие задачи (см. ниже).

### Sprint breakdown

| Sprint | Содержимое | Статус |
|---|---|---|
| 0.0 | Cargo workspace + ADR-0001…0006 + CI workflow + Nix flake + justfile + REUSE compliance | ✅ done |
| 0.1 | CI gates на GitHub Actions + lakko-proto codegen + lakko-crypto primitives (32 тестов) | ✅ done |
| 0.2 | lakko-mls поверх OpenMLS (identity / key_package / group / exporter) + end-to-end Alice ↔ Bob ↔ Charlie | ✅ done |
| 0.3 | lakko-transport (QUIC + in-memory) + lakko-ffi-uniffi (Identity / Group surface) | ✅ done |
| 0.4 | Fuzz harnesses (cargo-fuzz) на парсеры; reproducible build verifier (dual runner) | pending |
| 0.5 | KeyTransparency log skeleton + X.509 credentials в lakko-mls (FIPS suite unblocking) | pending |

### Что **не** в скоупе Phase 0
- Сервисы (DS, auth, storage) — Phase 1
- Реальные клиенты (iOS / Android / web / desktop) — Phase 1+
- Production cert management — Phase 1
- Anti-censorship транспорты — Phase 5
- Channels / broadcasts / calls — Phase 3+

### Exit criteria
- ✅ MLS groups работают end-to-end в integration тестах
- ✅ FFI surface стабилен (lakko-ffi-uniffi компилируется, three-party test через FFI зелёный)
- ✅ CI gates все зелёные
- ⏳ Reproducible build verifier — два независимых runner-а в CI сходятся в byte-identical
- ⏳ Fuzz harnesses на каждый парсер (MLS wire, protobuf, SFrame)

### Audits
Нет. Первый внешний аудит — Phase 2 exit.

### Top risks
- **OpenMLS upstream churn** — митигировано: fast-follow upgrades (мы уже сделали 0.6 → 0.8 при появлении hpke-rs CVE)
- **iOS reproducible builds** — Apple App Store re-signing ломает byte-identicality; решаем через Phase 5 (публикуем reproducible **unsigned** IPA как proof + signed App Store-артефакт как отдельный trust root)
- **uniffi 0.28 stale deps** (`paste`, `bincode`) — отмечены в `deny.toml` как ignore с привязкой к upstream

---

## Phase 1 — 1×1 Alpha (months 4–9)

### Цель

Первый продуктовый срез: два реальных устройства (iOS и Android) обмениваются 1×1-сообщениями через настоящий backend, с push-уведомлениями и ContactRequest-flow.

### Скоуп

#### Backend services
- **`services/auth`** — OPAQUE-PAKE (RFC draft `opaque`) + WebAuthn passkey, signup/signin без хранения PII; PoW (Equix) + Privacy Pass (RFC 9576) anti-spam
- **`services/ds`** — MLS Delivery Service: append-only журнал per-group, FIFO по `(epoch, seq)`, mailbox per device, FoundationDB или ScyllaDB (выбор внутри Phase 1)
- **`services/storage`** — ciphertext blob proxy перед SeaweedFS / S3, content-addressable
- **`services/discovery`** — username table + OPRF (RFC 9497) blinded lookup, exact-match only, PoW per-query
- **`services/push-gw`** — APNs + FCM + UnifiedPush envelope routing, payload только `(group_pseudonym, envelope_pointer)`

#### Crates (расширение)
- **`lakko-storage`** — SQLCipher-backed local store, ключ из HKDF от device root key (Secure Enclave / StrongBox)
- **`lakko-transport`** — production QUIC client wired через `services/ds`, cert pinning skeleton
- **`lakko-contacts`** — ContactRequest end-to-end, capability-based invite links (см. ADR-0005)
- **`lakko-push`** — encrypted push payload framing
- **`lakko-presence`** — blinded per-conversation presence tokens (минимальная версия)
- **`lakko-sync`** — hybrid logical clock + draft sync (single-device variant)

#### Apps
- **`apps/ios`** — SwiftUI: signup/signin (passkey + OPAQUE), contact list, 1×1 chat, attachments, push, recovery (BIP39 24-word)
- **`apps/android`** — Compose: тот же surface, ConnectionService и FCM integration

#### Infra
- Internal staging environment (Hetzner Cloud или Bare Metal), TLS via Let's Encrypt
- Build pipeline: XCFramework (Apple) + AAR (Android) генерация в CI, подписанные релизы (cosign + Sigstore)

#### Bootstrapping
- Key Transparency log — skeleton implementation (CONIKS-style фундамент); пока stub validation, реальные inclusion proofs в Phase 2
- X.509 credentials в lakko-mls (если не закрыто в Phase 0.5) — обязательно для KT log integration

### Что **не** в скоупе Phase 1
- Multi-device per user (Phase 2)
- Группы (Phase 2)
- Голос / видео (Phase 3)
- Каналы / broadcasts (Phase 4)
- Anti-censorship транспорты (Phase 5)
- Desktop / web клиенты (Phase 2)
- watchOS (Phase 5)

### Sprint breakdown

| Sprint | Содержимое |
|---|---|
| 1.1 | Auth service skeleton (OPAQUE + WebAuthn) + storage service + DS skeleton |
| 1.2 | iOS app: signup → key store → first chat skeleton (mock DS) |
| 1.3 | Android app: signup → key store → first chat skeleton |
| 1.4 | DS production: FoundationDB integration, mailbox protocol, fanout |
| 1.5 | ContactRequest end-to-end через DS, invite links (capability blobs) |
| 1.6 | 1×1 chat через MLS over DS, encrypted attachments через storage service |
| 1.7 | Push notifications: encrypted envelopes на APNs / FCM, UnifiedPush self-host для CN/RU prep |
| 1.8 | Recovery flow: BIP39 mnemonic + Shamir 3-of-5 social recovery (опционально) |
| 1.9 | Internal staging deployment + dogfooding 4+ недели |
| 1.10 | Hardening + crypto audit prep (документация инвариантов, threat model доделать) |

### Exit criteria
- 1×1 messaging работает end-to-end на реальных iOS + Android устройствах
- Внутренняя команда использует Lakko как daily messenger ≥4 недели
- Push delivery p50 < 5s
- Message send→delivered p50 < 500ms regional
- Reproducible build artifacts проверены на двух независимых runner-ах
- DS выдерживает 10k одновременных connection-ов в load test
- Recovery работает: можно потерять устройство и восстановить аккаунт по mnemonic

### Audits
Нет внешнего. Внутренний security review перед Phase 1 → 2 переходом обязателен.

### Top risks
- **iOS background limitations** — APNs delivery надёжность; MLS state machine recovery после долгой spavnki
- **DS storage layer choice** — FoundationDB ops complexity vs ScyllaDB familiarity; решаем эмпирически в 1.4
- **Hardware-backed key APIs** — iOS Secure Enclave vs Android StrongBox vs Android Keystore TEE-only; разные failure modes
- **OPAQUE production maturity** — RFC ещё draft; track upstream

---

## Phase 2 — Groups + Multi-device + Desktop/Web (months 9–15)

### Цель

Feature-complete messaging до 1000-человек групп, работает на всех 5 платформах (iOS / Android / macOS / Linux / Web), multi-device sync без plaintext на сервере.

### Скоуп

#### Crypto / protocol
- **X.509 credentials с KT log inclusion proofs** (replacing BasicCredential) — обязательно
- **Key Transparency log** — реальная CONIKS-style implementation, append-only, signed roots
- **FIPS ciphersuite** (P-384 + AES-256-GCM + ECDSA-P384) wired через X.509 credentials
- **DeviceGroup** — каждое устройство = leaf в личной MLS-группе пользователя
- **Identity rotation** через KT log supersession proof + 30-day bridge для старых invite links

#### Multi-device sync
- DeviceGroup как substrate для read-receipts, drafts, conversation order
- History catch-up bundle: HPKE-encrypted ciphertext от старого устройства новому
- Encrypted backups в SeaweedFS, KEK = Argon2id от recovery passphrase
- Optional Shamir 3-of-5 social recovery — guardian shares зашифрованы на identity key guardian-а

#### Группы
- Vanilla MLS до 1000 членов
- Group invite links (capability-based, ADR-0005)
- Approval flow для приватных групп
- Group settings (название, аватар — всё зашифрованное)

#### Discovery / privacy
- Все 6 уровней discoverability scope: Open / ContactsOfContacts / Contacts / InviteOnly / Allowlist / VerifiedNetwork
- Mutual-contact attestation chain для ContactsOfContacts
- Block list (зашифрован под identity, server применяет hashed match)

#### Apps (новые)
- **`apps/desktop`** — Tauri 2.x + Solid.js, Windows / macOS / Linux native binaries
- **`apps/web`** — Solid.js + WASM core, service worker для offline, IndexedDB cache
- **`apps/macos`** — отдельная Mac Catalyst-free сборка через SwiftUI XCFramework (общий с iOS core)

#### QR / safety numbers
- Signal-style safety numbers
- QR sharing для add-contact OOB-secure
- TOFU pinning через KT log + QR override

#### Pre-audit hardening
- `cargo fuzz` continuous integration (OSS-Fuzz)
- `kani` model checking на MLS state machine
- `proptest` свойства на crypto primitives
- Coverage ≥80% на lakko-crypto + lakko-mls

### Что **не** в скоупе Phase 2
- Каналы > 50k (Phase 4)
- Звонки (Phase 3)
- watchOS (Phase 5)
- Recording (Phase 4 или позже)

### Sprint breakdown

| Sprint | Содержимое |
|---|---|
| 2.1 | X.509 credentials + KT log skeleton (CONIKS-style append-only log) + FIPS suite unblock |
| 2.2 | DeviceGroup + multi-device sync substrate (read receipts, drafts) |
| 2.3 | History catch-up bundle exchange + encrypted backup в SeaweedFS |
| 2.4 | Shamir 3-of-5 social recovery |
| 2.5 | Desktop app: Tauri 2 + Solid.js + WASM core integration |
| 2.6 | Web app: Solid.js + service worker + IndexedDB cache |
| 2.7 | macOS app (XCFramework reuse) |
| 2.8 | Группы до 1000 + group invite links + approval flow |
| 2.9 | Все 6 discoverability scopes + mutual-contact attestation chain |
| 2.10 | QR safety numbers + block list (encrypted ACL) |
| 2.11 | Pre-audit hardening: cargo-fuzz, kani, coverage, mutation testing |
| 2.12 | **Внешний аудит + remediation** перед public beta |

### Exit criteria
- Feature-complete messaging на всех 5 платформах (iOS / Android / macOS / Linux / Web)
- Группа на 1000 членов стабильна в load test (Add/Remove под нагрузкой)
- Multi-device sync convergent в p95 < 5s
- ≥80% statement coverage на lakko-crypto + lakko-mls
- End-to-end тесты на iOS / Android / Web в CI зелёные
- WASM bundle <2MB gzipped
- **Внешний аудит проведён, все findings закрыты или с задокументированной mitigation**

### Audits
**Первый внешний аудит** — обязателен перед public beta. Покрывает: lakko-crypto, lakko-mls, lakko-storage, auth flows, KT log integration, recovery flows. Phase 2 не закрывается до remediation findings. Findings публикуются на сайте после fix.

### Top risks
- **KT log spec ещё draft** — возможен fork / patch upstream
- **WASM bundle size** для web (target <2MB gzipped) — может потребовать code splitting
- **Cross-device sync convergence** под flaky networks
- **OpenMLS X.509 credential maturity** — может потребовать upstream contributions
- **Audit findings блокируют timeline** — резервируем 2-3 sprint-а на remediation

---

## Phase 3 — Calls (months 15–22)

### Цель

Аудио / видео-звонки (P2P + групповые, по линку и в чате), качество **на уровне Telegram / Zoom / Yandex Telemost**.

### Скоуп

#### Backend
- **`services/sfu`** — форк LiveKit (Go), модифицирован под MLS-aware SFrame routing, ALPN `lakko-sfu/0`
- ICE / STUN / TURN с TURN-over-TLS fallback
- coturn self-hosted в каждом регионе
- Регионально распределённые SFU для low-latency

#### Crates
- **`lakko-calls`** — SFrame (RFC 9605) implementation, jitter buffer (NetEQ-style), codec negotiation, ICE/STUN/TURN client
- **`lakko-mls::group`** — расширение exporter API для call media keys
- **`lakko-transport`** — UDP-friendly path для STUN/TURN

#### Топологии звонков
- **P2P 1×1** — pure WebRTC + ICE/STUN/TURN, никакого SFU
- **Групповой по линку** — SFU + ad-hoc MLS group, join по embedded one-time KeyPackage
- **Групповой в чате** — SFU + reuse существующей chat MLS group, exporter `("lakko/call/{id}", member_idx, 32)`

#### Кодеки и качество
- Audio: Opus 32-64 kbps + DTX + FEC
- Video: AV1-SVC (modern hardware) / VP9-SVC (fallback) / H.264 (Apple legacy)
- Adaptive bitrate by SFU based on receiver BWE
- Бюджет: audio p50 < 80 ms regional / < 200 ms cross-continental, p95 < 250 ms
- Jitter buffer 20–120 ms adaptive

#### Apple integration
- CallKit для нативного UI входящих
- PushKit для wake-up на incoming call
- Live Activities для in-progress call indicator

#### Android integration
- ConnectionService для system-managed incoming UI
- MediaSession для controls
- ForegroundService для long-running call

#### Дополнительно
- Screen sharing — один screen producer, pinned layout
- In-call chat — обычные MLS application messages в group call's MLS group

### Что **не** в скоупе Phase 3
- Broadcasts (Phase 4)
- Audio rooms (Phase 4)
- Recording (Phase 4 или позже)
- Streaming (Phase 4)

### Sprint breakdown

| Sprint | Содержимое |
|---|---|
| 3.1 | LiveKit fork: ALPN, MLS-aware E2EE plumbing замена их default schema |
| 3.2 | P2P 1×1 calls: WebRTC + ICE/STUN/TURN, никакого SFU; integration с MLS exporter |
| 3.3 | SFU group calls по линку: ad-hoc MLS group, join по KeyPackage |
| 3.4 | SFU group calls в чате: reuse существующей chat MLS group |
| 3.5 | SFrame integration end-to-end с MLS exporter, key rotation на Add/Remove с 1s overlap |
| 3.6 | CallKit / PushKit / Live Activities (iOS) |
| 3.7 | ConnectionService / MediaSession / ForegroundService (Android) |
| 3.8 | Screen sharing |
| 3.9 | Codec optimization + adaptive bitrate + BWE feedback loop |
| 3.10 | Регионально распределённые SFU |
| 3.11 | Quality A/B test против Telegram / Zoom / Telemost в lab |
| 3.12 | Pre-audit hardening + SFrame integration аудит |

### Exit criteria
- P2P + group call quality A/B-tested против Telegram/Zoom/Telemost в lab; результат не хуже p50 metric
- Audio p50 < 80 ms regional, p95 < 250 ms
- 50-человек group call стабилен ≥1 час
- Screen sharing работает на всех платформах
- Все типы звонков **strict E2EE** (никакого silent fallback)
- Pre-launch audit findings закрыты

### Audits
**Targeted SFrame integration audit** — покрывает MLS exporter → SFrame key derivation → key rotation семантика → wire format. Не полный re-audit крипто, только интеграционный слой.

### Top risks
- **AV1 hardware decode** на старых устройствах (fallback на H.264)
- **iOS background CPU limits** во время длинных звонков
- **Cascade SFU latency** в дальних регионах
- **TURN abuse** — self-hosted coturn нужен rate-limit + auth
- **CallKit / ConnectionService** API differences create UX inconsistency между iOS и Android

---

## Phase 4 — Channels + Broadcasts + Audio Rooms (months 22–30)

### Цель

Telegram-parity content surfaces: каналы (1M+ подписчиков), broadcasts (1-to-many звонки), audio rooms (Clubhouse-style).

### Скоуп

#### Channels (>50k подписчиков)
Vanilla MLS не масштабируется к миллионам — реализуем **two-tier** архитектуру (см. план §3 / ADR-0001):

- **PublisherGroup** — vanilla MLS, ≤500 владельцев + модераторов + проверенных авторов; здесь происходят commit-ы и ротация **content key**
- **Sender-key broadcast** — подписчики получают current content key через HPKE-envelope на свой KeyPackage; контент канала шифруется этим ключом (AES-256-GCM + per-message nonce из sender-key chain)
- Подписчики **не являются** криптографическими членами группы
- Ротация content key — на membership change PublisherGroup и по расписанию (24h / N сообщений)
- Best-effort revoke подписчика — до следующей ротации
- **Документируется публично:** канал не имеет backward secrecy для контента (та же модель что Telegram/Slack/Discord, но мы делаем ротацию агрессивной)

#### Broadcasts (1-to-many звонки)
- Origin SFU + cascade на geographically distributed edge SFUs
- Аудитория получает encrypted media frames; не является членом MLS-группы звонка
- Reactions через отдельный канал-подобный sender-key flow
- Late-joiners получают current content key

#### Audio Rooms (Clubhouse-style)
- **Speakers** subgroup — vanilla MLS (≤50 speakers), частые rekey-и при role changes
- **Audience** — sender-key recipients (как в каналах)
- Promotion audience → speaker = MLS Add to Speakers + key rotation
- Demotion speaker → audience = MLS Remove + новый content key для audience

#### Reactions / comments в каналах
- Каждый high-engagement post — отдельная динамически создаваемая small MLS group для комментариев
- PublisherGroup как bootstrap для подписи initial commit

#### Backend
- **`services/edge`** — broadcast cascade origin → edge fan-out, audio room signaling
- Глобальная edge SFU топология (region-affinity routing)
- Sender-key envelope distribution через DS

### Что **не** в скоупе Phase 4
- Federation (post-launch)
- Recording channels / broadcasts (post-launch если будет policy-clarity)
- Streaming в external platforms (post-launch)

### Sprint breakdown

| Sprint | Содержимое |
|---|---|
| 4.1 | PublisherGroup в lakko-mls + content-key derivation API |
| 4.2 | Sender-key envelope distribution через DS (HPKE-to-KeyPackage) |
| 4.3 | Channel subscribe / unsubscribe flow + content-key rotation на schedule + membership change |
| 4.4 | Broadcast origin SFU + edge cascade topology в `services/edge` |
| 4.5 | Audio room: Speakers MLS subgroup + Audience sender-key |
| 4.6 | Audience ↔ Speaker promotion / demotion с key rotation |
| 4.7 | Reactions + comments на channel posts (per-post small MLS group) |
| 4.8 | Late-joiner flow для broadcasts |
| 4.9 | Load testing: 100k-subscriber канал, 1M-viewer broadcast, 100-speaker audio room |
| 4.10 | Pre-audit: scaling cryptography (sender-key, content-key rotation, revocation семантика) |
| 4.11 | **Внешний scaling crypto audit + remediation** |

### Exit criteria
- 100k-subscriber channel стабилен под нагрузкой
- 1M-viewer broadcast протестирован end-to-end
- Audio room: 100 speakers + 10k audience стабилен
- Документировано публично: backward-secrecy property каналов (no forward secrecy для контента)
- Promotion / demotion в audio room < 500 ms p95
- Scaling crypto audit findings закрыты

### Audits
**Scaling crypto audit** — фокус на: sender-key model security, content-key rotation policy, revocation semantics (best-effort vs cryptographic), audience size limits, edge cascade integrity.

### Top risks
- **Sender-key distribution scaling** — большой канал = много HPKE-envelope-ов на ротации; bandwidth budget на DS
- **Audio room latency** для глобальной аудитории (cascade hop добавляет ~50-100ms)
- **Revocation после раздачи content key** — best-effort до ротации; community reaction на этот факт
- **DS load** при rotation event для крупных каналов (можем нуждаться в throttling / staggered rollout)

---

## Phase 5 — Anti-censorship + Telegram-parity Launch (months 30–36)

### Цель

Публичный запуск с anti-censorship hardening для пользователей в Russia / Iran / China.

### Скоуп

#### Pluggable transports (см. ADR-0004)
- **REALITY** (xtls/REALITY) — TLS-in-TLS под чужой сертификат; defeats SNI-blocking + active probing
- **MASQUE / CONNECT-UDP** (RFC 9298) — туннель QUIC внутри HTTP/3
- **Shadowsocks-2022** с AEAD-2022 ciphers
- **VLESS + XTLS-Vision** — альтернатива REALITY
- **WebTransport** для web-клиента
- **obfs4** — legacy compatibility
- **Domain fronting** через Cloudflare / Fastly / Akamai / CloudFront — multi-CDN, considered deprecated

#### Bootstrap rendezvous
- Hardcoded DoH (Cloudflare 1.1.1.1, Google, Quad9, Mullvad) → ротирующийся пул innocuous-looking доменов
- ECH-protected (RFC 9460/9525) подписанные bootstrap configs (Ed25519, JSON: endpoints + working PT configs)
- Fallback: DNS-over-Tor → static anycast IP крупных CDN → out-of-band QR-код
- TUF (The Update Framework) signed transport configs — новые транспорты как **данные**, не код
- Auto-discovery: happy-eyeballs racing, top-2 рабочих держим warm, переключение на деградации, бюджет ≤10s cold start

#### Backend
- **`services/transport-rendezvous`** — производит signed bootstrap configs
- Multi-CDN edges (Cloudflare / Fastly / Akamai)
- Edge nodes в censorship-friendly регионах (Iran-adjacent, Hong Kong, Russia-adjacent jurisdictions)

#### Mobile constraints
- iOS — pluggable transports встраиваем в URLSession+QUIC, **не** через NEPacketTunnelProvider (50MB cap, нет фонового CPU)
- Опциональный VPN-mode возможен, но не default
- Android — VpnService доступен, но default тоже in-app

#### watchOS
- Thin presenter, общается с phone через paired secure channel
- Показывает уведомления, краткие чаты, голосовые сообщения
- Самостоятельно MLS для групп >50 не запускает

#### Localization
- 10+ языков (русский, английский, фарси, китайский упрощённый, арабский, испанский, португальский, немецкий, французский, японский, корейский, украинский, …)
- RTL layouts

#### Polish
- Onboarding flow (минимум friction, secure-by-default galage)
- Accessibility (VoiceOver / TalkBack полная поддержка)
- Анимации, переходы, восприятие "instant" UI

#### Launch infrastructure
- Public website (lakko.app)
- Landing page + signup
- Docs site
- Status page
- Bug bounty program (HackerOne или self-hosted)

### Что **не** в скоупе Phase 5
- Federation (post-launch)
- Post-quantum migration (track ML-KEM IETF work)
- Recording / streaming (post-launch policy review)

### Sprint breakdown

| Sprint | Содержимое |
|---|---|
| 5.1 | REALITY + Shadowsocks-2022 в lakko-transport как pluggable transport adapters |
| 5.2 | MASQUE / CONNECT-UDP (RFC 9298) |
| 5.3 | VLESS + XTLS-Vision; WebTransport для web |
| 5.4 | ECH (RFC 9460) + bootstrap rendezvous service |
| 5.5 | TUF-signed transport config distribution |
| 5.6 | Multi-CDN fronting glue (Cloudflare / Fastly / Akamai) |
| 5.7 | Happy-eyeballs racing logic + transport health monitoring |
| 5.8 | watchOS app (thin presenter) |
| 5.9 | Localization для 10+ языков, RTL support |
| 5.10 | UX polish: onboarding, accessibility, animations |
| 5.11 | Censorship lab testing — GFW emulation (gfw.report tools), RU TSPU emulation, IR DPI emulation |
| 5.12 | **Финальный pre-launch external audit** + remediation |
| 5.13 | Bug bounty launch (4+ недели pre-public) |
| 5.14 | Soft launch (invite-only) — 2-4 недели наблюдения |
| 5.15 | Public launch + press + scaling readiness |

### Exit criteria
- Успешные подключения из ≥5 censorship-emulating environments (GFW / TSPU / IR DPI / VR-restricted / aggressive corporate firewall)
- 99.9% uptime в pre-launch staging ≥30 дней
- Все audit findings закрыты или с публично disclosed mitigation
- Bug bounty live ≥4 недель без critical findings
- WatchOS app shipped
- Localization 10+ языков с native review
- Public launch ready: web, docs, status, support

### Audits
**Финальный pre-launch full-surface audit** — покрывает все: crypto, transport, infra, mobile apps, web, KT log, recovery flows, anti-censorship transport stack. Findings публично disclosed после remediation.

### Top risks
- **Censorship arms race after launch** — нужны rapid TUF-signed transport rollouts (operational readiness)
- **Press attention attracts state-level adversaries** — ops-sec posture для команды (отдельный SECURITY.md процесс)
- **Legal pressure** в странах с key disclosure laws (UK IPA, AU AA Act, India CERT-In, RU Yarovaya) — архитектура такая что компания **не может** исполнить требование под принуждением, но компанию могут терроризировать
- **iOS Network Extension limitations** ограничивают anti-censorship на iOS — fallback на in-app transports
- **Multi-CDN fronting deprecation** — Cloudflare / Fastly / Akamai периодически прикрывают; multi-CDN + planning for fronting death
- **DPRK-style allowlist-only internet** — сервис там недоступен; **abandon plan**: не инженерим под этот кейс

---

## Post-Launch (вне скоупа этого roadmap)

После Phase 5 normal product roadmap takes over. Известные направления, которые нужно отслеживать:

- **Federation** — между Lakko-инстансами; на этапе launch не делаем (single-tenant)
- **Post-quantum** — MLS extensions для ML-KEM в работе IETF; следить, мигрировать когда зрелое и доступно в OpenMLS
- **libcrux replacement** — переход на формально верифицированный crypto provider (формально верифицирован vs OpenMlsRustCrypto который полагается на audited-but-not-formally-verified RustCrypto)
- **OpenMLS upstream contributions** — резерв 1 FTE на upstream после launch
- **SDK split** — выделить `lakko-mls` / `lakko-crypto` / `lakko-proto` под Apache 2.0 в отдельный sub-repo если индустрия востребует embedded MLS implementation; благодаря CLA это возможно без переподписания контрибьюторов
- **Annual third-party audit** — обязателен после launch
- **Bug bounty** — продолжается без перерывов
- **Compliance certifications** — потенциально SOC 2, ISO 27001 если требуется enterprise customers (с осторожностью — каждый certification = audit-овой surface)

---

## Что важно помнить про этот документ

- **Сроки приблизительные** — реальный темп зависит от размера команды и сложности интеграции с upstream (OpenMLS, LiveKit, RustCrypto)
- **Audit timing критичен** — нельзя выходить в public beta или launch без external audit
- **Sprint breakdown** — это ориентир, не контракт; реальные sprint-ы планируются ad-hoc на 2-недельных циклах внутри фазы
- **Cross-phase dependencies** — нельзя начинать Phase N+1 пока exit criteria Phase N не зелёные
- **Документ обновляется** — после закрытия каждой фазы добавляем `### Retrospective` секцию с тем что прошло как планировалось vs что нет
