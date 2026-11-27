// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! ADR-0073 R.2: the acceptance rule for a contact's account-key **reset**,
//! as one pure decision, with rule 4 of the 2026-11-26 (second) amendment
//! (the reset seal, [ADR-0101] section 10).
//!
//! Until 2026-11-26 this table was written three times, in the web, iOS and
//! Android clients (`reset-verdict.ts`, `ResetVerdict.swift`,
//! `ResetVerdict.kt`). It is defined here once, on both FFI surfaces, so the
//! seal rule cannot drift between them; the clients gather the inputs and act
//! on the outcome.
//!
//! A reset is a key change with no supersession proof (with one, ADR-0016's
//! path runs and this rule is never asked). The caller runs the checks that
//! touch keys and proofs and hands this function their results:
//!
//! - `attest`: `verifyKeyResetAttestation` over the bundle's
//!   `key_reset_attest` under the trusted attest key;
//! - `log`: the verified key-log answer for the fetched key;
//! - `seal_signature`: `lakko_attest::check_reset_seal` (the previous
//!   generation's announcement under the held key, its `seq` floor, the
//!   reset seal and its announced key);
//! - `seal_log`: `lakko_transparency::verify_announcement_before_reset` (the
//!   KT-log half of rule 4).
//!
//! | # | attestation | generation              | key log                          | seal                                   | verdict                 |
//! |---|-------------|-------------------------|----------------------------------|----------------------------------------|-------------------------|
//! | 0 | -           | key unchanged           | -                                | -                                      | no verdict              |
//! | 1 | absent / invalid | -                  | any                              | -                                      | refused                 |
//! | 2 | no key      | -                       | any                              | -                                      | no verdict              |
//! | 3 | verified    | != bundle's, or <= accepted | any                          | -                                      | refused                 |
//! | 4 | verified    | ok                      | confirmed, `is_reset` false      | -                                      | refused (`unexplained`) |
//! | 5 | verified    | ok                      | mismatch / rollback              | -                                      | refused                 |
//! | 6 | verified    | ok                      | unreachable / absent             | -                                      | no verdict              |
//! | 7 | verified    | ok                      | confirmed + `is_reset`, or off   | signature invalid                      | **unsealed**            |
//! | 8 | verified    | ok                      | confirmed + `is_reset`           | signed, log half not proven            | **unsealed**            |
//! | 9 | verified    | ok                      | confirmed + `is_reset`           | signed, log half unreachable           | no verdict              |
//! | 10 | verified   | ok                      | confirmed + `is_reset`           | signed, log half proven                | **sealed**              |
//! | 11 | verified   | ok                      | off                              | signed                                 | **sealed** (rollback residual) |
//!
//! A refusal's copy is `unexplained` exactly when the log is `confirmed`
//! without `is_reset`, `unconfirmed` otherwise. Rows 7 and 11 rest on
//! decision D3 ([`LOG_OFF_ACCEPTS_RESET`]): with no key log in the build a
//! reset is judged without it, and the announcement rests on the held key's
//! signature and the device's `seq` floor (ADR-0101 section 10's stated
//! residual).
//!
//! **Sealed** is the old `accepted`: re-pin, R.5 to R.7, no UI. **Unsealed**
//! is a new account under the same username (ADR-0101 section 11): no
//! re-pin onto the old contact, nothing moved, nothing returned.
//!
//! [ADR-0101]: ../../../docs/adr/0101-reaching-a-reset-account.md

/// Decision D3 (ADR-0073 R.10): with no key log configured in the build, a
/// reset is judged on the attestation and the seal alone. `false` would
/// refuse every reset until the log is deployed.
pub const LOG_OFF_ACCEPTS_RESET: bool = true;

/// What the attestation check found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttestCheck {
    /// The bundle carries no attestation.
    Absent,
    /// It carries one and the verifier refused it, or it does not decode.
    Invalid,
    /// No attest key could be obtained: nothing can be said yet.
    NoKey,
    /// It verified for this account and the fetched key.
    Verified {
        /// The attested generation.
        key_generation: u64,
        /// The attestation's mint time, unix milliseconds.
        signed_at_ms: i64,
    },
}

/// The verified key-log answer for the fetched key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogStatus {
    /// The log holds the fetched key as the latest entry.
    Confirmed,
    /// The log holds no entry for the account.
    Absent,
    /// The served root is older than one already seen.
    Rollback,
    /// The log disagrees with the fetched key.
    Mismatch,
    /// The log could not be reached.
    Unreachable,
    /// The build has no key log.
    Off,
}

/// The log answer and its `is_reset` marker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LogCheck {
    /// The answer.
    pub status: LogStatus,
    /// The latest entry's `is_reset` marker, when one was read.
    pub is_reset: Option<bool>,
}

/// What `lakko_attest::check_reset_seal` found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SealSignature {
    /// The previous announcement and the reset seal verified.
    Valid {
        /// The announcement's `seq`; the caller raises its kept floor to it
        /// on a sealed verdict.
        announcement_seq: u64,
    },
    /// Either was missing or did not verify.
    Invalid,
}

/// What `lakko_transparency::verify_announcement_before_reset` found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SealLog {
    /// The served announcement is in the log before the reset leaf.
    Proven,
    /// The log was read and does not show it (no leaf, a later index, a
    /// proof that does not bind).
    NotProven,
    /// The log could not be reached for the announcement.
    Unreachable,
}

/// Which refusal copy the client shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResetRefusal {
    /// The key log shows the new key and does not record a reset.
    Unexplained,
    /// Everything else that is negative.
    Unconfirmed,
}

/// The outcome of R.2.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResetVerdict {
    /// Rules 1 to 4 hold: the same person, re-pin and move everything.
    Sealed {
        /// The attested generation.
        key_generation: u64,
        /// The attestation's mint time.
        signed_at_ms: i64,
        /// The previous announcement's `seq`.
        announcement_seq: u64,
    },
    /// Rules 1 to 3 hold and rule 4 does not: a new account under the same
    /// username.
    Unsealed {
        /// The attested generation.
        key_generation: u64,
        /// The attestation's mint time.
        signed_at_ms: i64,
    },
    /// The fail-closed refusal.
    Refused(ResetRefusal),
    /// Nothing can be decided yet.
    NoVerdict,
}

/// The facts of one fetched bundle the rule reads.
#[derive(Clone, Copy, Debug)]
pub struct ResetVerdictInput<'a> {
    /// The key this device holds for the account.
    pub held_key: &'a [u8],
    /// The generation this device last accepted (0 when none was recorded).
    pub accepted_generation: u64,
    /// The fetched bundle's key.
    pub fetched_key: &'a [u8],
    /// The fetched bundle's generation.
    pub bundle_generation: u64,
    /// The attestation check.
    pub attest: AttestCheck,
    /// The key-log check.
    pub log: LogCheck,
    /// The seal signature check.
    pub seal_signature: SealSignature,
    /// The seal's KT-log check (ignored when the log is off).
    pub seal_log: SealLog,
}

const fn refusal_for(log: LogCheck) -> ResetVerdict {
    let unexplained =
        matches!(log.status, LogStatus::Confirmed) && matches!(log.is_reset, Some(false));
    ResetVerdict::Refused(if unexplained {
        ResetRefusal::Unexplained
    } else {
        ResetRefusal::Unconfirmed
    })
}

/// R.2 with rule 4, in the order of the table in the module docs.
#[must_use]
pub fn reset_verdict(input: &ResetVerdictInput<'_>) -> ResetVerdict {
    if input.fetched_key == input.held_key {
        return ResetVerdict::NoVerdict;
    }
    let (key_generation, signed_at_ms) = match input.attest {
        AttestCheck::Absent | AttestCheck::Invalid => return refusal_for(input.log),
        AttestCheck::NoKey => return ResetVerdict::NoVerdict,
        AttestCheck::Verified {
            key_generation,
            signed_at_ms,
        } => (key_generation, signed_at_ms),
    };
    if key_generation != input.bundle_generation || key_generation <= input.accepted_generation {
        return refusal_for(input.log);
    }
    let log_off = match input.log.status {
        LogStatus::Confirmed if input.log.is_reset == Some(true) => false,
        LogStatus::Confirmed | LogStatus::Mismatch | LogStatus::Rollback => {
            return refusal_for(input.log)
        },
        LogStatus::Off if LOG_OFF_ACCEPTS_RESET => true,
        LogStatus::Off => return ResetVerdict::Refused(ResetRefusal::Unconfirmed),
        LogStatus::Unreachable | LogStatus::Absent => return ResetVerdict::NoVerdict,
    };
    let unsealed = ResetVerdict::Unsealed {
        key_generation,
        signed_at_ms,
    };
    let SealSignature::Valid { announcement_seq } = input.seal_signature else {
        return unsealed;
    };
    let sealed = ResetVerdict::Sealed {
        key_generation,
        signed_at_ms,
        announcement_seq,
    };
    if log_off {
        return sealed;
    }
    match input.seal_log {
        SealLog::Proven => sealed,
        SealLog::NotProven => unsealed,
        SealLog::Unreachable => ResetVerdict::NoVerdict,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HELD: [u8; 32] = [1; 32];
    const NEW: [u8; 32] = [2; 32];

    fn base() -> ResetVerdictInput<'static> {
        ResetVerdictInput {
            held_key: &HELD,
            accepted_generation: 1,
            fetched_key: &NEW,
            bundle_generation: 2,
            attest: AttestCheck::Verified {
                key_generation: 2,
                signed_at_ms: 10,
            },
            log: LogCheck {
                status: LogStatus::Confirmed,
                is_reset: Some(true),
            },
            seal_signature: SealSignature::Valid {
                announcement_seq: 5,
            },
            seal_log: SealLog::Proven,
        }
    }

    const SEALED: ResetVerdict = ResetVerdict::Sealed {
        key_generation: 2,
        signed_at_ms: 10,
        announcement_seq: 5,
    };
    const UNSEALED: ResetVerdict = ResetVerdict::Unsealed {
        key_generation: 2,
        signed_at_ms: 10,
    };
    const UNCONFIRMED: ResetVerdict = ResetVerdict::Refused(ResetRefusal::Unconfirmed);

    #[test]
    #[allow(clippy::too_many_lines)]
    fn the_whole_table() {
        let v = |i: ResetVerdictInput<'_>| reset_verdict(&i);
        // 0
        assert_eq!(
            v(ResetVerdictInput {
                fetched_key: &HELD,
                ..base()
            }),
            ResetVerdict::NoVerdict
        );
        // 1
        assert_eq!(
            v(ResetVerdictInput {
                attest: AttestCheck::Absent,
                ..base()
            }),
            UNCONFIRMED
        );
        assert_eq!(
            v(ResetVerdictInput {
                attest: AttestCheck::Invalid,
                ..base()
            }),
            UNCONFIRMED
        );
        // 2
        assert_eq!(
            v(ResetVerdictInput {
                attest: AttestCheck::NoKey,
                ..base()
            }),
            ResetVerdict::NoVerdict
        );
        // 3
        assert_eq!(
            v(ResetVerdictInput {
                bundle_generation: 3,
                ..base()
            }),
            UNCONFIRMED
        );
        assert_eq!(
            v(ResetVerdictInput {
                accepted_generation: 2,
                ..base()
            }),
            UNCONFIRMED
        );
        // 4
        let not_reset = LogCheck {
            status: LogStatus::Confirmed,
            is_reset: Some(false),
        };
        assert_eq!(
            v(ResetVerdictInput {
                log: not_reset,
                ..base()
            }),
            ResetVerdict::Refused(ResetRefusal::Unexplained)
        );
        assert_eq!(
            v(ResetVerdictInput {
                attest: AttestCheck::Absent,
                log: not_reset,
                ..base()
            }),
            ResetVerdict::Refused(ResetRefusal::Unexplained)
        );
        let unknown_marker = LogCheck {
            status: LogStatus::Confirmed,
            is_reset: None,
        };
        assert_eq!(
            v(ResetVerdictInput {
                log: unknown_marker,
                ..base()
            }),
            UNCONFIRMED
        );
        // 5
        for status in [LogStatus::Mismatch, LogStatus::Rollback] {
            assert_eq!(
                v(ResetVerdictInput {
                    log: LogCheck {
                        status,
                        is_reset: None
                    },
                    ..base()
                }),
                UNCONFIRMED
            );
        }
        // 6
        for status in [LogStatus::Unreachable, LogStatus::Absent] {
            assert_eq!(
                v(ResetVerdictInput {
                    log: LogCheck {
                        status,
                        is_reset: None
                    },
                    ..base()
                }),
                ResetVerdict::NoVerdict
            );
        }
        // 7
        assert_eq!(
            v(ResetVerdictInput {
                seal_signature: SealSignature::Invalid,
                ..base()
            }),
            UNSEALED
        );
        let off = LogCheck {
            status: LogStatus::Off,
            is_reset: None,
        };
        assert_eq!(
            v(ResetVerdictInput {
                log: off,
                seal_signature: SealSignature::Invalid,
                ..base()
            }),
            UNSEALED
        );
        // 8
        assert_eq!(
            v(ResetVerdictInput {
                seal_log: SealLog::NotProven,
                ..base()
            }),
            UNSEALED
        );
        // 9
        assert_eq!(
            v(ResetVerdictInput {
                seal_log: SealLog::Unreachable,
                ..base()
            }),
            ResetVerdict::NoVerdict
        );
        // 10
        assert_eq!(v(base()), SEALED);
        // 11: the log half is not asked when the log is off.
        for seal_log in [SealLog::Proven, SealLog::NotProven, SealLog::Unreachable] {
            assert_eq!(
                v(ResetVerdictInput {
                    log: off,
                    seal_log,
                    ..base()
                }),
                SEALED
            );
        }
    }

    #[test]
    fn an_unsealed_reset_never_outranks_a_refusal() {
        // A reset the old rule refused stays refused, whatever the seal says.
        let v = reset_verdict(&ResetVerdictInput {
            attest: AttestCheck::Invalid,
            seal_signature: SealSignature::Invalid,
            seal_log: SealLog::NotProven,
            ..base()
        });
        assert_eq!(v, UNCONFIRMED);
    }
}
