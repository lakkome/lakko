<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# ADR-0003: Форк LiveKit как SFU; SFrame для медиа

- **Статус:** Accepted
- **Дата:** 2026-04-28
- **Контекст:** Phase 0 (решение фиксируется заранее, реализация — Phase 3)

## Контекст

Lakko должен поддерживать звонки сравнимого с Telegram/Zoom/Yandex Telemost качества: P2P 1×1, групповые (до десятков участников), broadcasts (миллионы зрителей), audio rooms (несколько speakers + аудитория). Все звонки — E2EE-защищены: сервер видит только зашифрованные frame-ы.

Нужен production-grade Selective Forwarding Unit с simulcast/SVC и интеграцией E2EE-keys из MLS.

## Решение

### SFU — форк [LiveKit](https://github.com/livekit/livekit) (Go, Apache 2.0 → форк под AGPLv3)

Используется как база; форкаем под AGPLv3 в `services/sfu/`. Модифицируем для:

- Привязки SFrame ключей к MLS exporter (вместо out-of-band ключей LiveKit)
- Нашего auth/handshake протокола (через QUIC или WebTransport)
- Cascade-топологии для broadcasts (Phase 4)

### Шифрование медиа — SFrame (RFC 9605)

Per-frame AES-128-GCM с nonce из counter. Hardware AES-acceleration (AES-NI / ARMv8 Crypto Extensions) — overhead <0.001% CPU на 60fps@1080p.

Ключи получаются один раз в начале звонка через `mls_exporter("lakko/call/v1", call_id, 32)` → HKDF → per-participant SFrame keys по leaf index. Ротация при Add/Remove с 1-секундным overlap (старый ключ дослушивается).

### Топологии звонков

| Тип | Топология | MLS group | Media keys |
|---|---|---|---|
| P2P 1×1 | Pure WebRTC + ICE/STUN/TURN | Существующая 1×1 group | exporter из неё |
| Групповой по линку | LiveKit SFU | Ad-hoc group, join по embedded one-time KeyPackage | exporter call group |
| Групповой в чате | LiveKit SFU | Reuse существующей chat MLS group | `exporter("lakko/call/{id}", idx, 32)` |
| Broadcast | Origin SFU + cascade на edge SFU | PublisherGroup (presenter) + sender-key для аудитории | content key, ротация на membership change |
| Audio room | LiveKit SFU | `Speakers` MLS subgroup + `Audience` (sender-key) | promotion = MLS Add = key rotation |

### Кодеки

- **Audio:** Opus 32-64 kbps + DTX + FEC
- **Video:** AV1-SVC (modern), VP9-SVC (fallback), H.264 (Apple legacy)

### Бюджет качества

- Audio p50 < 80 ms regional / < 200 ms cross-continental, p95 < 250 ms
- Video 30 fps @ 720p baseline, 60 fps @ 1080p при ширине канала
- Adaptive jitter buffer 20-120 ms (NetEQ-style)
- SVC layer drop на SFU по BWE-репортам receiver-а

## Альтернативы

### mediasoup

Зрелый Node.js SFU. Отлично работает, но Node.js-rooted ecosystem не вяжется с Rust/Go стеком; интеграция в наш auth/control plane потребует постоянных мостов.

**Отброшено:** в долгосрочной перспективе heterogeneity backend-стека дороже форка одного компонента.

### Janus / Jitsi Videobridge

Зрелые SFU. Минусы: C (Janus) или Java (Jitsi); архитектура заточена под room-based модель Jitsi Meet, наши broadcast-topologies требуют значительных изменений.

**Отброшено:** меньше совпадает с нашей моделью звонков чем LiveKit.

### Кастомный SFU с нуля на Rust

Свобода дизайна. Минусы: 2 человеко-года до production-grade, простая E2EE через SFrame не оправдывает написание собственного SFU.

**Отброшено:** не наш core differentiator. Форк LiveKit с заменой E2EE-плумбинга на MLS — несколько человеко-месяцев.

### Использовать LiveKit как библиотеку, без форка

LiveKit Cloud SaaS или self-host upstream. Минусы: их license (Apache 2.0) не даёт защиты от proprietary-форка; их E2EE-плумбинг не привязан к MLS.

**Отброшено:** AGPLv3-форк необходим для consistency лицензии (см. ADR-0006); MLS-интеграция в любом случае требует наших изменений.

## Последствия

### Положительные

- Production-grade SFU с simulcast/SVC, ICE/TURN, network resilience — за счёт upstream LiveKit code
- E2EE через стандарт SFrame (RFC 9605) — interop возможен с другими SFrame-совместимыми клиентами
- MLS exporter → SFrame keys — единая identity и group management между chat и call
- Hardware AES → нулевая CPU-нагрузка на крипто во время звонка
- Cascade-friendly архитектура для broadcast scaling

### Отрицательные

- Форк требует поддержки upstream-changes (cherry-pick fixes) или дивергенции
- Go в backend-стеке (LiveKit) добавляется к Rust → две toolchain в CI
- Наша AGPL-версия SFU не сможет потреблять proprietary LiveKit Cloud features

### Нейтральные

- Дополнительная задержка от SFU forwarding (5-15 ms) приемлема для всех типов звонков

## Открытые вопросы

- Упростимся ли мы (Phase 3+) до single-codec AV1 везде, или fallback к H.264 для Apple останется навсегда?
- Recording звонков — как это должно работать с E2EE? Открытый вопрос на Phase 4
- TURN-as-a-Service vs self-hosted coturn — Phase 3 решит

## Ссылки

- [LiveKit](https://github.com/livekit/livekit)
- [RFC 9605 — Secure Frame (SFrame)](https://www.rfc-editor.org/rfc/rfc9605)
- ADR-0001 — MLS exporter для media keys
- ADR-0006 — лицензирование (объясняет необходимость форка)
