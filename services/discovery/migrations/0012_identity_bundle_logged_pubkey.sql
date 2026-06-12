-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- KT-log S2.5 — transparency-log reconciliation marker.
--
-- S2 appends an identity-key CHANGE to the transparency log BEST-EFFORT,
-- fail-open: a transparency-side failure (service down, non-2xx, network
-- error) never fails the discovery upload, so a key change can slip
-- through unlogged. S2.5 closes that gap with a background reconciler that
-- eventually appends every account's CURRENT published identity to the
-- log, so the log converges to "contains every account's latest key".
--
-- `logged_pubkey` is the per-account marker the reconciler keys off:
--
--   * NULL                            -> never confirmed in the log (a
--                                        first publish whose hot-path
--                                        append failed, or a row that
--                                        predates this column).
--   * <> the current signature_pubkey -> the account's key changed but the
--                                        change was not confirmed in the
--                                        log (the hot-path append failed).
--   * == the current signature_pubkey -> the current key is CONFIRMED in
--                                        the log; the reconciler skips it.
--
-- The hot-path upload sets it to the new signature_pubkey AFTER a
-- successful transparency append; the reconciler advances it after a
-- successful (idempotent) append. A failed write leaves it stale so the
-- next sweep retries. NULL/stale -> the reconciler re-appends.
--
-- This is a discovery-side bookkeeping column only; the transparency
-- service stays opaque and never learns about it. It holds the same
-- public Ed25519 key already stored in `signature_pubkey`, so it leaks no
-- secret (invariant #2 untouched).
ALTER TABLE identity_bundles
    ADD COLUMN logged_pubkey BINARY(32) NULL
        COMMENT 'KT-log S2.5: the signature_pubkey last CONFIRMED in the transparency log; NULL/stale -> the reconciler re-appends';
