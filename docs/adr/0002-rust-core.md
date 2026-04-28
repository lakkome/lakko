<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# ADR-0002: Shared Rust core + native UI на каждой платформе

- **Статус:** Accepted
- **Дата:** 2026-04-28
- **Контекст:** Phase 0

## Контекст

Lakko должен работать на iOS, iPadOS, macOS, watchOS, Android, Windows, Linux и Web с одинаковым поведением крипто, протокола, network-стека и хранилища. Параллельно — нативный UX на каждой платформе (CallKit, Live Activities, MediaSession, Compose, SwiftUI), чтобы качество и отзывчивость были на уровне Telegram/Zoom.

Любое расхождение в crypto/MLS-логике между платформами = риск protocol-level баг, ломающий interop или вводящий security regression.

## Решение

**Shared Rust core (Cargo workspace) + native UI per platform**.

### Core

Все crates в `crates/`:

- `lakko-core` (umbrella) re-exports публичный API
- `lakko-crypto`, `lakko-mls`, `lakko-proto`, `lakko-transport`, `lakko-storage`, `lakko-sync`, `lakko-calls`, `lakko-contacts`, `lakko-presence`, `lakko-push` — domain crates
- `lakko-ffi-uniffi` — Swift / Kotlin биндинги через [UniFFI](https://github.com/mozilla/uniffi-rs)
- `lakko-ffi-wasm` — Web/Tauri биндинги через [wasm-bindgen](https://rustwasm.github.io/wasm-bindgen/)

`#![forbid(unsafe_code)]` на всех domain crates; unsafe разрешён только в `lakko-ffi-*` с обязательным `// SAFETY:` комментарием.

### Per-platform UI

| Платформа | UI-стек | Биндинг |
|---|---|---|
| iOS / iPadOS / macOS / watchOS | SwiftUI + платформенные API (CallKit, Live Activities, PushKit, AVFoundation) | XCFramework через UniFFI |
| Android | Jetpack Compose + платформенные API (MediaSession, ConnectionService, FCM) | AAR через UniFFI |
| Windows / macOS / Linux desktop | Tauri 2.x + Solid.js | Native Rust integration в host process |
| Web | Solid.js + WASM | wasm-bindgen |

Solid.js выбран и для desktop, и для web из-за fine-grained reactivity (signals) — точечные обновления UI без перерисовок, что соответствует security-критичному инварианту "no full-screen reloads" (силок для phishing/UI redress).

### watchOS — частный случай

Полное MLS-состояние для многих групп тяжело держать на ватче. `apps/watchos` будет thin presenter, общающийся с phone через paired secure channel; самостоятельно MLS для групп >50 не запускает. Подробности — отдельный ADR в Phase 5.

## Альтернативы

### Flutter + Rust core

Flutter для всех клиентов + Rust core. Плюс: единый UI-код для iOS/Android/desktop. Минусы: компромиссный нативный UX, watchOS вообще не поддерживается, проблемы с Live Activities / CallKit / MediaSession (всё через platform channels).

**Отброшено:** требование "качество UX на уровне Telegram" с нативной интеграцией в системные сервисы перевешивает экономию на UI-коде.

### React Native + Electron + Web (общий TypeScript)

Максимум переиспользования TS-кода. Минусы: JS-крипто на клиенте, Electron-overhead, watchOS отдельной кодобазой, security-аудит сложнее (V8 + Node attack surface).

**Отброшено:** для security-проекта неприемлемо иметь крипто на JS, и Electron-десктоп проигрывает по памяти и нативности.

### Полностью native (Swift / Kotlin / C++)

Лучший UX, но дублирование крипто/протокол-логики на 3+ языках — недопустимый риск protocol-level расхождений и удвоение audit-овой поверхности.

**Отброшено:** unified core критичен для security; UX обеспечивается тем, что **UI** native, а не всё.

## Последствия

### Положительные

- Единая крипто-логика → одна точка аудита и формальной верификации
- Невозможны inter-platform protocol-divergence баги (single source of truth — `lakko-mls`)
- Нативный UX на каждой платформе через свой native UI-стек
- Tauri вместо Electron → 30× меньшие бинарники, нет лишнего runtime
- AI-агентный workflow (см. CLAUDE.md) хорошо ложится на Cargo workspace

### Отрицательные

- FFI-границы (UniFFI, wasm-bindgen) — поверхность ошибок, которые сложно поймать в тестах
- Каждое breaking-изменение публичного API core → перегенерация биндингов и обновление всех клиентов
- Команда нуждается в Rust + Swift/Kotlin/TS экспертизе одновременно
- Билд-pipeline сложнее: XCFramework + AAR + WASM + Tauri target — 4 разных артефакта на каждый релиз core

### Нейтральные

- watchOS как thin presenter — компромисс по функциональности, нормальный для smart-watch формата

## Открытые вопросы

- Как версионировать FFI surface — semver или содержательный contract test? (Phase 1 решит)
- Десктоп: Tauri 2.x достаточно зрелый? Если упрёмся — fallback на native Win32/Cocoa/GTK + Rust biz-logic. Решим к концу Phase 2

## Ссылки

- [UniFFI — Rust → Swift/Kotlin/Python](https://github.com/mozilla/uniffi-rs)
- [wasm-bindgen](https://rustwasm.github.io/wasm-bindgen/)
- [Tauri 2.0](https://tauri.app/)
- [Solid.js — fine-grained reactivity](https://www.solidjs.com/)
- ADR-0001 — MLS as crypto core
- ADR-0006 — лицензирование (AGPL влияет на FFI-границы)
