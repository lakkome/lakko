<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# ADR-0004: Pluggable transports для anti-censorship

- **Статус:** Accepted
- **Дата:** 2026-04-28
- **Контекст:** Phase 0 (фиксируется заранее, реализация в Phase 5)

## Контекст

Lakko должен работать в Russia / Iran / China и других регионах с DPI-блокировками. Адверсар может:

- Блокировать по SNI / IP / ASN
- Применять active probing (Great Firewall стиль)
- Отключать UDP (анти-QUIC)
- Динамически блокировать домены

Domain fronting (использование шифрования SNI к одному домену + Host-header к другому через CDN) исторически работал, но Cloudflare (2015), Fastly (2018), Google Cloud (2018) официально его прикрыли. Полагаться на него опасно.

## Решение

**Multi-transport архитектура:** клиент шипит набор pluggable transports, в runtime выбирает работающий через happy-eyeballs racing.

### Edge listeners (все слушают на 443)

- **TLS 1.3 + ECH** (RFC 9460/9525) — TCP/443
- **QUIC v1** ([quinn](https://github.com/quinn-rs/quinn)) — UDP/443
- **WebSocket-over-TLS** — TCP/443 fallback
- **MASQUE / CONNECT-UDP** (RFC 9298) — туннелирование QUIC внутри HTTP/3, побеждает UDP-блокировку

### Pluggable transports (в порядке приоритета)

1. **REALITY** ([xtls/REALITY](https://github.com/XTLS/REALITY)) — TLS-in-TLS под чужой реальный сертификат; defeats SNI-blocking + active probing. Ключевой против GFW.
2. **Shadowsocks-2022** с AEAD-2022 ciphers
3. **VLESS + XTLS-Vision** — альтернатива REALITY
4. **WebTransport** — H3-based, для web-клиента
5. **obfs4** — legacy, для совместимости с Tor-like окружением
6. **Domain fronting** через Cloudflare/Fastly/Akamai/CloudFront — multi-CDN, weighted random; считаем deprecated, планируем смерть, не основа

### Bootstrap rendezvous (chicken-and-egg)

1. Hardcoded DoH-резолверы (Cloudflare 1.1.1.1, Google 8.8.8.8, Quad9, Mullvad) → резолв ротирующегося пула innocuous-looking доменов
2. Каждый домен возвращает **ECH-protected подписанный bootstrap config** (Ed25519, JSON: текущие endpoints + working PT configs)
3. Fallback: DNS-over-Tor → статические anycast IP крупных CDN → out-of-band QR-код для extreme cases

### Auto-discovery

Happy-eyeballs racing нескольких транспортов параллельно; top-2 рабочих держим warm; переключение на деградации; бюджет ≤ 10 секунд cold start.

### Mobile constraints

- **iOS:** pluggable transports встраиваем в URLSession+QUIC, **не** через NEPacketTunnelProvider (50MB memory cap, нет фонового CPU). Опциональный VPN-mode возможен, но не default.
- **Android:** VpnService доступен, но default — in-app транспорт.

### Pluggable transports как **данные**, не как код

Конфиги транспортов подписаны через [TUF (The Update Framework)](https://theupdateframework.io/) и доставляются через signed updates. Новый транспорт можно добавить без переcompиляции клиента. Старые конфиги ротируются автоматически.

## Альтернативы

### Только Domain Fronting

Минусы: CDN-провайдеры систематически отключают; переменная политика; SNI-extraction делает fronting видимым.

**Отброшено:** не основа в 2026+. Считаем бонусом, не фундаментом.

### Только Tor / только обфусцированный Tor

Минусы: высокая latency несовместима с realtime-звонками; Tor exit nodes блокируются массово.

**Отброшено:** Tor не подходит для realtime media. Можно использовать как escape hatch для control-plane сообщений.

### Стандартный QUIC без обфускации

Минусы: GFW и TSPU блокируют QUIC по характеристикам пакетов; SNI/ESNI всё равно нужно прятать.

**Отброшено:** недостаточно против active censors.

### Полностью свой обфусцированный протокол

Минусы: будет быстро обнаружен и заблокирован по характерным паттернам, потому что окажется единственным таким протоколом.

**Отброшено:** мимикрия под уже distributed протоколы (TLS handshake, HTTP/3) безопаснее, чем уникальный паттерн.

## Последствия

### Положительные

- Adversary должен заблокировать всё, что мимикрирует Lakko (Cloudflare, Apple iCloud, etc.) — это политически дорого
- Pluggable transports как signed data → можно добавлять новые транспорты в production без code release
- Multi-CDN bootstrap снижает single-point-of-failure
- TUF-signed configs защищают от хищной подмены transport configs

### Отрицательные

- Постоянная гонка вооружений: адверсар учится detect-ить, мы добавляем новые транспорты
- Multi-transport racing увеличивает cold start latency на 2-5 секунд
- iOS Network Extension limitations не дают full VPN-mode без UX-компромисса
- В extreme regions (DPRK-style allowlist-only internet) сервис не доступен — abandon plan: не инженерим под этот кейс

### Нейтральные

- TUF-pipeline для transport configs требует отдельного key management (отдельно от release signing)

## Открытые вопросы

- Как отслеживать deployment эффективность транспортов в production без деанонимизации пользователей? Telemetry-без-PII через differential privacy — Phase 5
- Стоит ли встраивать Tor как ultimate fallback для control plane (signaling, push)? Phase 5
- Stockpile of "innocuous" доменов для bootstrap — нужна постоянная ротация, отдельный legal/operational ресурс

## Ссылки

- [REALITY — XTLS](https://github.com/XTLS/REALITY)
- [RFC 9298 — CONNECT-UDP / MASQUE](https://www.rfc-editor.org/rfc/rfc9298)
- [RFC 9460 — Encrypted Client Hello](https://www.rfc-editor.org/rfc/rfc9460)
- [TUF — The Update Framework](https://theupdateframework.io/)
- [GFW Report — censorship research](https://gfw.report/)
- ADR-0002 — клиенты (где живут pluggable transports)
