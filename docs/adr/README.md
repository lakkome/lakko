<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# Architecture Decision Records

ADR — Architecture Decision Record. Каждый файл фиксирует **одно** значимое решение, мотивацию, альтернативы и последствия. Решения — иммутабельны: если позиция меняется, пишется новый ADR со статусом `Supersedes 000X` и старый помечается `Superseded by 000Y`.

## Формат

См. [`0000-template.md`](0000-template.md). Стиль — Michael Nygard, минималистично.

## Индекс

| # | Статус | Заголовок |
|---|---|---|
| [0001](0001-mls-only.md) | Accepted | MLS (RFC 9420) для всех типов разговоров |
| [0002](0002-rust-core.md) | Accepted | Shared Rust core + native UI на каждой платформе |
| [0003](0003-livekit-fork.md) | Accepted | Форк LiveKit как SFU, SFrame для медиа |
| [0004](0004-anti-censorship.md) | Accepted | Pluggable transports: REALITY + MASQUE + Shadowsocks-2022 |
| [0005](0005-contact-invitations.md) | Accepted | Capability-based invites + 6 сценариев + pseudonym lifecycle |
| [0006](0006-licensing.md) | Accepted | AGPLv3 monorepo + CLA |
