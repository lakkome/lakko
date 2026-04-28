<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# ADR-0006: AGPLv3 для всего monorepo + CLA

- **Статус:** Accepted
- **Дата:** 2026-04-28
- **Контекст:** Phase 0

## Контекст

Lakko — security-критичный E2EE-мессенджер. Лицензия должна:

1. Защитить пользователей от closed-fork с backdoor-ом (атака "взяли наш код, добавили backdoor, продали как secure")
2. Не препятствовать аудитной прозрачности и open-source участию
3. Быть совместимой с App Store / Play Store
4. Быть простой в compliance-управлении в monorepo (один код, один LICENSE)
5. Сохранить опцию commercial dual-licensing в будущем

Repo — monorepo (см. CLAUDE.md), AI-агентный workflow.

## Решение

**AGPLv3 для всего кода в monorepo.** Один `LICENSE` в корне (полный текст AGPL-3.0). SPDX header `AGPL-3.0-or-later` обязателен в каждом файле.

**CLA (Contributor License Agreement)** обязательный для всех контрибьюторов — Apache ICLA-style. Проверяется ботом `cla-assistant` при каждом PR. Без подписанного CLA PR не мерджится.

CLA даёт project maintainer-у право переиспользовать вклад под другой лицензией в будущем — критическая опция для:
- Выделить отдельный SDK-крейт (например `lakko-mls`) под Apache 2.0 в Phase 4-5, если он станет привлекателен для индустрии
- Commercial dual-license для embedded / enterprise

### Tooling enforcement

- `REUSE.toml` (FSFE REUSE 3.0) — fallback annotations на случай файлов без header (binary fixtures, generated code)
- `reuse lint` в CI — блокирует PR без header / с неправильной лицензией
- `cargo deny` — блокирует deps с GPL-incompatible лицензиями (например, MS-RL, EPL без явного permission)
- Pre-commit hook добавляет SPDX header в новые файлы (template per language)

### Третьесторонние зависимости

`cargo deny` allow-list:
- MIT, Apache-2.0, BSD-2/3-Clause, ISC, MPL-2.0, Unicode-DFS-2016, Zlib, CC0-1.0, BSL-1.0
- AGPL-совместимые: AGPL-3.0+, GPL-3.0+, LGPL-3.0+

Не разрешены: проприетарные, GPL-2.0-only (несовместима с AGPL-3.0), CDDL, OSL-без-явного-permission.

## Альтернативы

### MIT / Apache 2.0 для всего

Максимальная экосистемная польза, но **никакой защиты** от closed fork с backdoor. Wire изначально был на Apache, потом перешёл на GPLv3 ровно по этой причине.

**Отброшено:** для security-критичного messenger неприемлемо. Это конкретный риск, не теоретический.

### Гибрид: Apache 2.0 на crypto/MLS/SDK + AGPLv3 на app/server

Element/Matrix-стиль. Плюсы: SDK можно встраивать в OSS-проекты; AGPLv3 защищает product-уровень. Минусы: compliance overhead в monorepo (REUSE per-file, направление зависимостей Apache ← AGPL только; cargo-deny gate; постоянная дисциплина).

**Отброшено для Phase 0-3:** на этой стадии нет реального консумера SDK; compliance overhead перевешивает спекулятивную пользу. CLA сохраняет опцию выделить SDK-crate под Apache в Phase 4-5 без переподписания контрибьюторов.

### Полностью AGPL без CLA (только DCO Linux-style)

Простота. Минусы: невозможно сменить лицензию в будущем без согласия каждого контрибьютора → ловушка.

**Отброшено:** CLA даёт необходимую гибкость. DCO достаточен для проектов с фиксированной лицензией навсегда; Lakko может захотеть commercial dual-license.

### GPLv3 (без Affero clause)

Защищает от closed binary distribution, но **не** от SaaS-фарма (адверсар хостит проприетарный fork как сервис без публикации). Для server-side кода неприемлемо.

**Отброшено:** AGPLv3 закрывает SaaS-loophole.

## Последствия

### Положительные

- Closed-source backdoored fork не сможет легально распространяться
- SaaS-фарм проприетарного fork-а обязан публиковать модификации
- Один LICENSE = тривиальная compliance в monorepo (vs гибрид)
- CLA сохраняет flexibility для future re-licensing
- Сильная позиция при формировании community — "мы за пользователей"
- App Store / Play Store совместимы с AGPL (FSF подтверждение, прецеденты Element X, Threema)

### Отрицательные

- Меньше потенциальных коммерческих consumer-ов SDK (не могут embed без AGPL-обязательств)
- CLA — административная нагрузка (cla-assistant bot, отказы PR без CLA)
- Некоторые контрибьюторы откажутся подписывать CLA принципиально → потеря части community

### Нейтральные

- AGPL-only код не может быть использован академическими работами под Apache/MIT (но они могут принять AGPL для своего форка)

## Открытые вопросы

- Юр.лицо проекта (copyright holder, CLA assignee) — Швейцария или Эстония? Решим к Phase 1; пока temporary "Lakko Contributors"
- Apache ICLA-vs-other формат CLA — final выбор после legal review
- В Phase 4-5: рассмотреть выделение `lakko-mls` под Apache 2.0 в отдельный subrepo, если индустрия востребует

## Ссылки

- [GNU Affero General Public License v3.0](https://www.gnu.org/licenses/agpl-3.0.html)
- [REUSE Specification 3.0](https://reuse.software/spec/)
- [Apache ICLA template](https://www.apache.org/licenses/contributor-agreements.html)
- [cla-assistant](https://github.com/cla-assistant/cla-assistant)
- ADR-0003 — фоrk LiveKit (необходимость AGPL объясняется здесь)
