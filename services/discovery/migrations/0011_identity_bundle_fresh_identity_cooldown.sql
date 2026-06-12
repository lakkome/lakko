-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- ADR-0017 defence-in-depth (lakko-security Phase-3A follow-up (ii)) — a
-- per-account cool-down on UNPROVEN AccountIdentity key changes, to blunt a
-- compromised-ACCESS attacker churning many fresh identities (the
-- lost-everything path publishes a CHANGED key with NO supersession proof).
--
-- Two columns:
--
--   * `previous_signature_pubkey` — the key that was current BEFORE the
--     latest change. Lets the server tell a HEAL (going back to the
--     immediately-previous key — e.g. the owner's warm-login re-publish
--     after an attacker replaced their key) from CHURN (a brand-new key).
--     A heal is ALWAYS allowed (never cool-down-gated) so a cool-down can
--     never lock a legitimate owner out of restoring their identity.
--   * `last_unproven_key_change_at` — the wall-clock of the last unproven
--     change to a NEW key. The upload handler refuses a further unproven
--     change to a new key within the configured window
--     (`LAKKO_DISCOVERY_FRESH_IDENTITY_COOLDOWN_SECS`, default 1 h; 0
--     disables). Proven rotations (a valid supersession proof) and
--     no-change re-uploads are NEVER gated.
--
-- The server still never PARSES the proof bytes (invariant #2); it only
-- observes whether a proof is present and whether the key changed — both
-- already implied by the bytes it must store. No new secret is held.
ALTER TABLE identity_bundles
    ADD COLUMN previous_signature_pubkey BINARY(32) NULL
        COMMENT 'ADR-0017 (ii): the signature_pubkey current before the latest change; lets a heal-back be distinguished from churn',
    ADD COLUMN last_unproven_key_change_at TIMESTAMP(3) NULL
        COMMENT 'ADR-0017 (ii): wall-clock of the last unproven change to a NEW key; the cool-down window is measured from this';
