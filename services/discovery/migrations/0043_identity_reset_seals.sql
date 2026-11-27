-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- ADR-0101 section 10 (the reset seal) and ADR-0073 "Amended 2026-11-26
-- (second)": a peer honours an account's key reset only when the reset is
-- sealed by a seal key the account announced, under its old account key,
-- before the reset.
--
-- `reset_seals`          the account's newest seal announcement of its
--                        current generation (200 to 641 bytes, signed by the
--                        identity key this row publishes);
-- `reset_seals_seq`      its `seq`, kept beside it so the KT reconciler can
--                        find an unlogged announcement without parsing;
-- `previous_reset_seals` the newest announcement of the previous generation,
--                        moved here when the generation moves (a reset or a
--                        proven rotation): the one a peer checks a reset
--                        against;
-- `reset_seal`           the 214-byte seal of the latest attested reset,
--                        beside `key_reset_attest`; cleared by a key change
--                        that carries none;
-- `logged_seals_seq`     the highest announcement `seq` the transparency log
--                        confirmed, so the reconciler appends the rest and a
--                        reset appends its previous announcement first.
--
-- Public artefacts signed by the account (invariant #2). Discovery checks
-- the signatures it can (the KT leaf needs the announcement's generation and
-- `seq`) and stores the bytes as they came. Nullable: an account that never
-- published one has none. No production deployment.

ALTER TABLE identity_bundles
    ADD COLUMN reset_seals VARBINARY(641) NULL,
    ADD COLUMN reset_seals_seq BIGINT UNSIGNED NULL,
    ADD COLUMN previous_reset_seals VARBINARY(641) NULL,
    ADD COLUMN reset_seal VARBINARY(214) NULL,
    ADD COLUMN logged_seals_seq BIGINT UNSIGNED NULL;
