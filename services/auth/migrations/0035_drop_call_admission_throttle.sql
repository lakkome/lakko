-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- ADR-0078 section 2 - calls admitted anonymously.
--
-- `POST /call/jwt` no longer takes a session: it spends an anonymous
-- `call-join` token, so this service does not know which account joins a
-- call and must not store one next to a call id. The two ADR-0033 tables
-- did exactly that (`call_admission_events` held `(call_id, account_id, at)`
-- for every young account's join). The rebuilt throttle is a per-room cap
-- held in memory (`src/room_cap.rs`) plus a small `call-join` quota for
-- young accounts at token issuance (`src/anon_tokens.rs`); nothing about a
-- call is written to this database any more.
DROP TABLE IF EXISTS call_admission_events;
DROP TABLE IF EXISTS throttled_calls;
