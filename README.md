<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# Lakko

Защищённый мессенджер с **E2EE по умолчанию** для всего: 1×1 чатов, групп, каналов, аудио/видео-звонков, audio rooms и broadcasts. Сервер zero-trust — даже при полной компрометации не позволяет расшифровать сообщения, историю, звонки и не раскрывает социальный граф.

> **Статус: Phase 0 — Foundations.** Репозиторий в активной разработке. Не используйте для реальных коммуникаций.

## Ключевые свойства

- **MLS (RFC 9420)** для всех видов разговоров — единый E2EE-протокол
- **SFrame (RFC 9605)** для шифрования медиа в звонках, ключи через MLS exporter
- **Zero PII** — телефоны и email не сохраняются вообще; только usernames для discovery
- **Discovery без раскрытия графа** — username lookup через OPRF blinded queries
- **Hardware-backed device keys** на всех платформах
- **Anti-censorship transport** — REALITY, MASQUE, Shadowsocks-2022, WebTransport
- **Все клиенты** на одном Rust core: iOS / iPadOS / macOS / watchOS / Android / Windows / Linux / Web

## Архитектура

Полный план — `~/.claude/plans/1-nested-wreath.md`. Архитектурные решения — в `docs/adr/`. Навигация по репо — `CLAUDE.md`.

## Лицензия

[AGPLv3](LICENSE). Все вклады принимаются под [CLA](CLA.md).

## Безопасность

Уязвимости — только через GPG-encrypted email, **не** через публичные issues. Адрес и ключ — в `SECURITY.md` (появится к Phase 1).
