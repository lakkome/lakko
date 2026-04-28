<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# ADR-0001: MLS (RFC 9420) для всех типов разговоров

- **Статус:** Accepted
- **Дата:** 2026-04-28
- **Контекст:** Phase 0

## Контекст

Lakko должен обеспечивать E2EE по умолчанию для:

- 1×1 чатов
- Групповых чатов до ~50 000 участников
- Каналов с миллионами подписчиков
- Аудио/видео-звонков (P2P, групповых, broadcasts, audio rooms)

Нужен единый криптопротокол с формальной верификацией, индустриальной поддержкой и эффективными групповыми операциями.

## Решение

**MLS (RFC 9420)** как единый E2EE-протокол для всего:

- 1×1 = degenerate MLS group of 2 (никаких отдельных протоколов вроде X3DH/Double Ratchet)
- Группы ≤ 50 000 — vanilla MLS, ratchet tree O(log n)
- Каналы > 50 000 — two-tier: vanilla MLS PublisherGroup (≤500 владельцев+модераторов) + sender-key broadcast подписчикам через HPKE-envelopes
- Звонки — медиа-ключи через `mls_exporter("lakko/call/v1", call_id, 32)` → SFrame (RFC 9605); ротация при Add/Remove с 1-секундным overlap

**Ciphersuites:**

- `MLS_256_DHKEMP384_AES256GCM_SHA384_P384` — FIPS-friendly путь
- `MLS_256_DHKEMX25519_CHACHA20POLY1305_SHA256_Ed25519` — быстрый путь по умолчанию

**Identity** — Ed25519, MLS X.509 credentials с self-signed root + публичный transparency log (CONIKS-style, draft `key-transparency`).

**Multi-device** — каждое устройство = отдельный leaf в DeviceGroup (личной MLS-группе пользователя). Add/Remove устройства = коммит в DeviceGroup → fan-out как Add/Remove proposals в каждый conversation group.

**Реализация:** [OpenMLS](https://github.com/openmls/openmls) (единственная зрелая Rust-имплементация); `lakko-mls` crate оборачивает её, фиксирует ciphersuites, изолирует upstream API. Бюджет 1 FTE на upstream-контрибуции / форк.

## Альтернативы

### Signal Double Ratchet + MLS только для групп

Классика для 1×1 (forward secrecy + post-compromise security проверены десятилетием). Минусы: две кодовые базы, два набора invariants, удвоение audit-овой поверхности, отсутствие PCS-ratcheting на уровне 1×1 при изменении состава устройств.

**Отброшено:** один протокол на всё значительно проще аудитировать и поддерживать; 1×1 как degenerate group имеет более чистую модель multi-device.

### Только MLS, без Double Ratchet, но без X.509 credentials (BasicCredential)

Минимализм. Минусы: нет встроенной revocation, нет расширяемости для transparency log без протокольных изменений.

**Отброшено:** X.509 credentials дают необходимый extensibility-хук для CONIKS-style transparency log без custom протокольных полей.

### Кастомный протокол на основе академических работ

Свобода дизайна. Минусы: годы на формальную верификацию, отсутствие interop, полная ответственность за крипто-баги.

**Отброшено:** не делаем своих криптопротоколов. Стандарт IETF лучше.

## Последствия

### Положительные

- Один протокол на 1×1 / группы / каналы / звонки → один audit, один state machine, один interop tooling
- IETF стандарт RFC 9420 → industry-wide interop возможен (с другими MLS-имплементациями: mlspp, libmls и др.)
- Эффективные групповые операции (O(log n) на Add/Remove) до 50k участников
- Готовая Rust-реализация (OpenMLS) экономит ~2 человеко-года

### Отрицательные

- Channels > 50k участников **не покрываются** vanilla MLS → требуется two-tier с sender-key broadcast → отказ от backward secrecy для контента канала (документируется публично)
- OpenMLS пре-1.0 → возможны breaking changes; нужны upstream-контрибуции
- MLS state machine сложнее Double Ratchet для 1×1 → требует более тщательного тестирования
- watchOS не сможет держать full MLS state для многих групп (см. ADR-0002)

### Нейтральные

- 1×1 ratchet medium-cost (vs Double Ratchet) — приемлемо в 2026+ на любом железе
- KeyPackages нужно регулярно ротировать (TTL 30 дней) — стандартное MLS-поведение

## Открытые вопросы

- Стратегия миграции на full MLS draft `mls-large` когда он стандартизуется — повлияет на channel-схему
- Federation между Lakko-инстансами — позже, не в Phase 0-2
- Post-quantum — MLS extensions для ML-KEM в работе IETF; следить, мигрировать когда зрелое

## Ссылки

- [RFC 9420 — The Messaging Layer Security (MLS) Protocol](https://www.rfc-editor.org/rfc/rfc9420)
- [RFC 9605 — Secure Frame (SFrame)](https://www.rfc-editor.org/rfc/rfc9605)
- [draft-ietf-keytrans-architecture (Key Transparency)](https://datatracker.ietf.org/doc/draft-ietf-keytrans-architecture/)
- [OpenMLS](https://github.com/openmls/openmls)
- ADR-0002 — клиентская архитектура
- ADR-0003 — SFU и звонки
