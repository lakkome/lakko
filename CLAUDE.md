<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# Lakko — навигация для AI-агента

Этот файл автоматически загружается в контекст каждой сессии. Держим его коротким и точным; детали — в `docs/adr/` и в плане `~/.claude/plans/1-nested-wreath.md`.

## Что такое Lakko

Защищённый мессенджер уровня Telegram с **E2EE по умолчанию для всего** (1×1, группы, каналы, audio/video звонки, broadcasts, audio rooms). Zero-trust сервер. Все клиенты — единое Rust-ядро + нативный UI.

## Архитектурные решения (single source of truth — `docs/adr/`)

| ADR | Решение |
|---|---|
| [0001](docs/adr/0001-mls-only.md) | MLS (RFC 9420) для всего — 1×1, группы, каналы, медиа-ключи звонков |
| [0002](docs/adr/0002-rust-core.md) | Shared Rust core + native UI на каждой платформе |
| [0003](docs/adr/0003-livekit-fork.md) | Форк LiveKit как SFU; SFrame (RFC 9605) для медиа |
| [0004](docs/adr/0004-anti-censorship.md) | REALITY + MASQUE + Shadowsocks-2022 + WebTransport |
| [0005](docs/adr/0005-contact-invitations.md) | Capability-based invites + 6 сценариев + pseudonym lifecycle |
| [0006](docs/adr/0006-licensing.md) | AGPLv3 monorepo + CLA |

Полный стратегический план и trade-offs — в `~/.claude/plans/1-nested-wreath.md`.
Тактическая декомпозиция всех 6 фаз с sprint-breakdown — в [`docs/ROADMAP.md`](docs/ROADMAP.md).

## Карта модулей

### `crates/` — Rust core

| Crate | Назначение |
|---|---|
| `lakko-core` | Umbrella, re-exports публичного API |
| `lakko-crypto` | HPKE, HKDF, AEAD, Argon2id, OPRF, BLAKE3 |
| `lakko-mls` | OpenMLS wrapper, group state, exporters для звонков |
| `lakko-proto` | Сгенерированные типы из `schemas/`, общие со server-ом |
| `lakko-transport` | quinn (QUIC) + tungstenite (WS) + pluggable obfs trait |
| `lakko-storage` | SQLCipher abstraction, blob store, WAL |
| `lakko-sync` | Multi-device sync, hybrid logical clock, edit CRDT |
| `lakko-calls` | SFrame, jitter buffer, codec negotiation, ICE/TURN |
| `lakko-contacts` | Username discovery (OPRF), invite capabilities, pseudonym lifecycle |
| `lakko-presence` | Blinded per-conversation presence tokens |
| `lakko-push` | Encrypted APNs / FCM / UnifiedPush envelopes |
| `lakko-transparency` | CONIKS-style key transparency log (Merkle tree, signed roots, inclusion proofs) |
| `lakko-ffi-uniffi` | UniFFI surface для Swift / Kotlin биндингов |
| `lakko-ffi-wasm` | wasm-bindgen surface для Web/Tauri |

### `services/` — Backend (Rust)

| Service | Назначение |
|---|---|
| `auth` | OPAQUE-PAKE + WebAuthn passkey, без хранения PII |
| `ds` | MLS Delivery Service (FIFO журнал per-group) |
| `storage` | Ciphertext blob proxy перед SeaweedFS |
| `edge` | Broadcast cascade + audio room signaling |
| `discovery` | Username lookup через OPRF |
| `push-gw` | Fanout APNs / FCM / UnifiedPush |
| `transport-rendezvous` | Подписанный bootstrap config для anti-censorship |

### Прочее

- `schemas/` — protobuf-схемы; **single source of truth** для wire format
- `apps/` — клиенты (iOS/iPadOS/macOS/watchOS/Android/desktop/web), появятся к Phase 1
- `infra/` — Terraform + Nomad/k8s manifests
- `docs/adr/` — Architecture Decision Records
- `tools/` — codegen, fuzz harnesses, бенчмарки
- `ci/` — GitHub Actions + self-hosted SLSA L3 builders

## Security invariants — нарушать запрещено

1. **Никакого plaintext PII на сервере.** Phone/email **никогда** не сохраняются — даже в зашифрованном виде. Только OPAQUE-blinded записи и hash для recovery.
2. **Сервер видит только ciphertext + публичные артефакты** (KeyPackages, transparency log entries). Никаких message keys, chain keys, root keys, media keys.
3. **MLS state machine sacrosanct.** Любое изменение `lakko-mls` требует ADR + property tests + (после Phase 2) внешний крипто-аудит.
4. **No fallback на не-E2EE.** Strict E2EE-only — handshake не завершился → connection не открывается. Никакого silent downgrade.
5. **Социальный граф не материализуется на сервере.** Add-contact = encrypted envelope на pseudonym; presence — blinded per-conversation tokens; discovery — OPRF blinded lookup.
6. **Identity rotation** — только через KT log supersession proof. Старые ключи не возвращаются.
7. **Hardware-backed device keys** обязательны на всех платформах (Secure Enclave / StrongBox / TPM 2.0 / non-extractable WebCrypto).

## Правила кодстайла

- **Каждый файл** имеет `SPDX-License-Identifier: AGPL-3.0-or-later` header. Rust: `// SPDX-…`; Markdown: `<!-- SPDX-… -->`. Без исключений. CI блокирует.
- **`#![forbid(unsafe_code)]`** на каждой crate, кроме `lakko-ffi-uniffi` и `lakko-ffi-wasm` (там обоснованный unsafe — задокументировать `// SAFETY:` комментарием).
- **Без `unwrap()` / `expect()`** в production-коде — только в тестах и benchmarks. Используем `?` и `thiserror` для ошибок.
- **Без `panic!()`** в либ-коде (можно в bin при unrecoverable startup error).
- **Без `println!` / `eprintln!`** — логирование через `tracing` crate.
- **`clippy::all`, `clippy::pedantic`, `clippy::nursery`** — все warning. `-D warnings` в CI.
- **`cargo fmt`** обязателен — pre-commit hook + CI gate.
- **Документация** на публичные элементы — `missing_docs` warning.
- **Не добавлять зависимости** без необходимости. Каждая dep — потенциальная supply-chain уязвимость. `cargo deny` блокирует unknown licenses.

## Команды (через justfile)

```
just check                     # cargo check --workspace --all-targets
just test                      # cargo test --workspace
just lint                      # cargo clippy --workspace -- -D warnings
just fmt                       # cargo fmt --all
just fmt-check                 # cargo fmt --all --check
just audit                     # cargo deny check
just reuse                     # reuse lint
just regen-proto               # перегенерация типов из schemas/
just regen-bindings            # Swift + Kotlin биндинги из lakko-ffi-uniffi
just fuzz-check                # smoke-build всех fuzz-targets на stable
just fuzz <crate> <target>     # запуск конкретного target-а на nightly + cargo-fuzz
just fuzz-list                 # все доступные fuzz-targets
just ci                        # всё что делает CI локально
```

## Fuzz harnesses

`crates/lakko-mls/fuzz/` и `crates/lakko-proto/fuzz/` — отдельные крейты вне основного workspace (нужен nightly + libfuzzer-sys). Targets живут в `fuzz/fuzz_targets/*.rs`.

Текущие targets:
- **`lakko-mls/mls_message`** — `MlsMessageIn` wire-format parser (TLS codec)
- **`lakko-mls/key_package`** — `KeyPackage` wire-format parser
- **`lakko-proto/wire_envelope`** — `wire::v1::Envelope` prost decoder
- **`lakko-proto/mls_app_payload`** — `mls_app::v1::ApplicationPayload` prost decoder
- **`lakko-proto/media_call_key`** — `media::v1::CallKeyEpoch` + `CodecCapability` decoders

`fuzz_api` модуль в `lakko-mls` (помечен `#[doc(hidden)]`) — единственный публичный entry-point для harness-ов; не часть supported public API.

Фуззинг-corpus не коммитится (`fuzz/corpus/`, `fuzz/artifacts/` в `.gitignore`); поднимается локально.

## Чеклист перед коммитом

- [ ] `just fmt` прошёл
- [ ] `just lint` прошёл (zero warnings)
- [ ] `just test` прошёл
- [ ] `just reuse` прошёл (SPDX headers на месте)
- [ ] Изменения в crypto/mls/proto имеют ADR-ссылку в commit message
- [ ] Изменения в schemas/ обратно совместимы (proto reserved fields, не удалять номера)
- [ ] Если security-relevant — добавлен property test или fuzz target

## Workflow для AI-агента

- **Plan mode** для архитектурных задач (новый crate, изменение протокола, schema migration)
- **TodoWrite** для multi-step задач (>3 шага)
- **Не закладывайся на будущие фичи** — пишем под конкретную задачу. Spec roadmap — в плане, реализуем по Phase
- **При сомнении про security-критичный код** — остановись, спроси, не угадывай
- **Subagents** (Explore, Plan) — для broad research, не для тривиальных правок
- **Не коммить без подписи** — `commit.gpgsign = true` обязателен на этом репо

## Технические инварианты

- **Rust edition 2021**, MSRV закреплён в `rust-toolchain.toml`
- **Cargo workspace** — один `Cargo.toml` в корне, все crates и services внутри
- **Workspace dependencies** — версии общих deps пинятся в `[workspace.dependencies]`, crates наследуют через `dep.workspace = true`
- **Reproducible builds** — Nix flake пинит весь toolchain; CI проверяет byte-identicality между runner-ами
