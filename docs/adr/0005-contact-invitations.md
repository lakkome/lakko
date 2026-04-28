<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# ADR-0005: Capability-based contact invitations + 6 сценариев + pseudonym lifecycle

- **Статус:** Accepted
- **Дата:** 2026-04-28
- **Контекст:** Phase 0

## Контекст

Discovery в Lakko работает только по `@username` — phone/email никогда не сохраняются (см. ADR-0001). Социальный граф **не материализуется** на сервере: server маршрутизирует ContactRequest по encrypted envelope, не видя identity отправителя/получателя.

Нужна полная схема приглашений, покрывающая 6 сценариев:

| # | Сценарий | Кто инициирует |
|---|---|---|
| A | Знаю `@username` — хочу добавить | Initiator |
| B | Дать кому-то "карточку" себя | Owner |
| C | Пригласить **нового** пользователя в Lakko | Owner |
| D | Личная встреча (in-person scan) | Both |
| E | Пригласить в **группу** | Group admin |
| F | Подписаться на **канал** | Subscriber |

При этом анти-stalking, анти-spam и privacy инварианты должны соблюдаться.

## Решение

Полная спецификация — в `~/.claude/plans/1-nested-wreath.md` §13 (раздел "Contact Invitations"). Здесь — ключевые архитектурные решения.

### Базовый flow ContactRequest (сценарий A)

```
Alice → OPRF(@bob) → discovery service → KeyPackageBundle(Bob) + KT proof
Alice строит ContactRequest {
    from_pubkey, from_pseudonym (fresh per recipient),
    HPKE_seal(B_pubkey, intro_message),
    PoW (Equix), Privacy Pass token,
    signature
}
→ DS на Bob's stranger-inbox mailbox.
Bob: pending UI → Accept | Decline | Block | Ignore
On Accept: обе стороны открывают MLS group of 2.
```

Сервер видит только `(pseudonym_A → pseudonym_B, opaque_envelope, PoW_token)`.

### Capability-based invite links (сценарии B/C)

URL: `https://lakko.app/i/<base64url-pointer>` — в URL только pointer + MAC, **сама** capability — encrypted blob в SeaweedFS:

```rust
struct InviteCapability {
    owner_identity_hash:        [u8; 32],          // BLAKE3(A_pubkey)
    owner_transparency_proof:   KTLogProof,
    ephemeral_prekey:           HpkePublicKey,
    blob_id:                    Sha256,
    expiry:                     UnixTimestamp,
    max_uses:                   u32,
    consumed_pubkey_log:        HmacBloom,         // server-side counter, ciphertext-blind
    bootstrap_referral:         Option<bool>,       // C: signup deep-link
    signature:                  Ed25519Sig,
}
```

Преимущества: revocable (delete blob), скриншот URL не выдаёт identity, server считает downloads без знания content.

UI-варианты ссылки:
- "Одноразовая, 7 дней" — default
- "До 10 использований, 7 дней" — для команд
- "До 30 дней" — для bio в соцсетях
- "Бессрочная" — заблокирована до verified-аккаунта (anti-stalking)

### QR / safety numbers (сценарий D)

```rust
struct QrPayload {
    identity_fingerprint:   [u8; 32],     // BLAKE3(pubkey)
    one_time_prekey:        HpkePublicKey,
    transparency_position:  u64,
    signature:              Ed25519Sig,
}
```

Двойное назначение: add new contact OOB-secure + verify existing contact (Signal-style safety numbers).

### Group invite (сценарий E)

```rust
struct GroupInvite {
    group_id_pseudonym,
    group_name_ciphertext,            // HPKE_seal с invite-key
    welcome_blob_id,                   // ephemeral KeyPackage от admin
    inviter_signature,
    expiry, max_uses,
}
```

При accept: joiner скачивает Welcome → MLS Add proposal → joiner становится leaf.

### Pseudonym lifecycle

| Тип | Lifecycle | Назначение |
|---|---|---|
| Identity pubkey | Постоянный | Основной ключ, в KT log; ротация — supersession proof |
| Per-relationship pseudonym | На время существования контакта | Server маршрутизирует им; ротируется по запросу |
| Stranger-inbox pseudonym | Долгоживущий, опционально-ротируемый | Приём cold ContactRequest; ротация рвёт недополученные приглашения |
| Per-invite-link pseudonym | Жизнь capability | Привязан к blob_id |

### Зафиксированные решения по развилкам

| Вопрос | Решение |
|---|---|
| Cancel pending ContactRequest | Можно до accept (клиентская "delete-for-everyone" на pending) |
| Default discoverability scope для новичков | **InviteOnly** — secure-by-default |
| Глобальный "online" / "last seen" | **Нет.** Presence только в MLS-группах через blinded tokens |
| Auto-import адресной книги | **Запрещено.** Адресная книга = PII; никогда не загружается |
| Контактные подгруппы ("коллеги", "семья") | DeviceGroup-encrypted list (server blind) |
| Identity key compromise / rotation | KT log supersession proof + 30-day bridge для старых invites |

### Anti-abuse pipeline

- Cold-contact rate per sender: ≤5/день для нового, ≤50/день для прогретого; PoW difficulty экспоненциально растёт
- Privacy Pass token (App Attest / Play Integrity) на каждый ContactRequest и каждую генерацию InviteCapability
- Receiver-side priority binning: незнакомцы — в low-priority inbox без push
- Per-recipient silent mute (sender не знает о блоке)
- Mass-complaint signal: N жалоб → server поднимает PoW для исходящих от pseudonym

## Альтернативы

### Phone-number-based discovery (как Signal/WhatsApp)

Phone — strong identity, network effect через автоматическое определение контактов. Минусы: PII на сервере, легко связать с личностью, social graph reconstruction через intersection attacks, требует SIM-карту (problem в RU/IR/CN).

**Отброшено:** прямое нарушение security invariants Lakko (no PII).

### Username + phone-hash для recovery

Гибрид: phone-hash хранится для recovery. Минусы: hash легко brute-force-ится (10^9 номеров), социальный граф восстанавливается через intersection.

**Отброшено:** hash phone-номеров — не security primitive, security theatre.

### Public key как identity (DID-style)

Identity = pubkey, никаких usernames. Плюсы: zero PII. Минусы: UX для поиска ужасный (32-байтный hash вместо @alice).

**Отброшено:** username-as-handle важен для UX; делаем username table с OPRF lookup (ADR-0001 + текущий ADR).

## Последствия

### Положительные

- Server видит **только** pseudonym → pseudonym маршрутизацию; социальный граф не реконструируется даже при полной компрометации
- 6 сценариев приглашений покрывают все realistic UX-кейсы
- Capability-based links revocable и rate-controlled
- Anti-abuse multi-layered (PoW + Privacy Pass + complaint signal)

### Отрицательные

- Cold-contact UX медленнее чем "phone autofind" — пользователь должен знать `@username`
- Pseudonym ротация stranger-inbox рвёт недополученные приглашения — fair trade-off
- "InviteOnly by default" может выглядеть как "никто не может меня найти" — нужен onboarding hint

### Нейтральные

- KT log inclusion proofs на каждый KeyPackage fetch — мелкие сетевые накладные расходы (~150-300 байт/lookup)

## Открытые вопросы

- Как реализовать **mutual-contact attestation chain** для `ContactsOfContacts` scope без leak-а transitive contacts на сервере? Cryptographic proof + client-side verification — детали в Phase 2
- Cross-device sync списка отправленных ContactRequest-ов через DeviceGroup — Phase 2
- Спам-detection при mass-complaint signal без знания identity — risk: targeted brigading. Митигировать proof-of-personhood (зрелые webauthn-аккаунты)?

## Ссылки

- [RFC 9497 — Oblivious Pseudorandom Functions (OPRF)](https://www.rfc-editor.org/rfc/rfc9497)
- [RFC 9576 — Privacy Pass](https://www.rfc-editor.org/rfc/rfc9576)
- [draft-ietf-keytrans-architecture](https://datatracker.ietf.org/doc/draft-ietf-keytrans-architecture/)
- [Equix proof-of-work (Tor onion services)](https://gitlab.torproject.org/tpo/core/tor/-/issues/40729)
- ADR-0001 — MLS / OPRF discovery
- Полная спецификация — `~/.claude/plans/1-nested-wreath.md` §13
