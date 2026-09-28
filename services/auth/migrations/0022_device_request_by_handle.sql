-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- ADR-0056 — a fresh device names its account by login handle, not by
-- account id.
--
-- device_requests: `POST /device/request` now takes a login handle and
-- resolves it as the ADR-0054 S5 name routes do. A handle nobody holds
-- resolves to a stable stand-in account id that is not in `accounts`; its
-- request is stored like a real one (no device ever polls it, so it simply
-- expires), which is what keeps `POST /device/request/status` from telling
-- a known name from an unknown one. The foreign key would refuse that row,
-- so it goes; account deletion removes the account's request explicitly
-- (`webauthn::delete_account`), and the request TTL bounds any leftover.
ALTER TABLE device_requests DROP FOREIGN KEY fk_device_requests_account;

-- device_grants: `POST /device/authorize/redeem` now takes the grant token
-- only and finds the account through `SHA-256(grant_token)`. The primary key
-- is `(account_id, grant_hash)`, whose leading column the lookup does not
-- have, so the hash gets its own index. UNIQUE: the hash is of 32 random
-- bytes, so two grants never share one, and a hash names at most one account.
ALTER TABLE device_grants ADD UNIQUE INDEX uq_device_grants_hash (grant_hash);
