// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Pure helpers of the moderators' panel ([`crate::panel`]): names on the
//! wire, dates and ISO weeks, the case queue fold, and the JSON of a logged
//! statement.

use std::collections::BTreeMap;

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use lakko_moderation::statement::{SignedStatement, Statement};
use lakko_moderation::{ModeratorList, Reason, TargetKind};
use serde_json::{json, Value};

use crate::log::{LEAF_MODERATOR_LIST, LEAF_SNAPSHOT_ROOT, LEAF_STATEMENT};
use crate::store::{CaseLock, CaseReasonAgg, CaseRow, CaseStatus};

/// Milliseconds in a day.
pub const DAY_MS: u64 = 86_400_000;

/// The wire name of a target kind.
#[must_use]
pub const fn kind_name(kind: TargetKind) -> &'static str {
    match kind {
        TargetKind::Account => "account",
        TargetKind::GroupHandle => "group-handle",
        TargetKind::Channel => "channel",
        TargetKind::Name => "name",
        TargetKind::Blob => "blob",
    }
}

/// The target kind of a wire name.
#[must_use]
pub fn kind_from_name(name: &str) -> Option<TargetKind> {
    [
        TargetKind::Account,
        TargetKind::GroupHandle,
        TargetKind::Channel,
        TargetKind::Name,
        TargetKind::Blob,
    ]
    .into_iter()
    .find(|k| kind_name(*k) == name)
}

/// The wire name of a stored target-kind byte.
#[must_use]
pub fn kind_byte_name(byte: u8) -> &'static str {
    TargetKind::from_u8(byte).map_or("unknown", kind_name)
}

/// The wire name of a stored reason byte.
#[must_use]
pub fn reason_byte_name(byte: u8) -> &'static str {
    Reason::from_u8(byte).map_or("unknown", Reason::name)
}

/// The reason of a wire name.
#[must_use]
pub fn reason_from_name(name: &str) -> Option<Reason> {
    Reason::ALL.into_iter().find(|r| r.name() == name)
}

/// The wire name of a conversation-kind byte.
#[must_use]
pub const fn conversation_kind_name(byte: u8) -> &'static str {
    match byte {
        1 => "one-to-one",
        2 => "group",
        3 => "channel",
        4 => "contact-request",
        5 => "discover",
        6 => "card",
        _ => "unknown",
    }
}

/// Lower-case hex.
#[must_use]
pub fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// Decode lower- or upper-case hex.
#[must_use]
pub fn unhex(text: &str) -> Option<Vec<u8>> {
    let text = text.trim();
    if text.len() % 2 != 0 {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| {
            text.get(i..i + 2)
                .and_then(|p| u8::from_str_radix(p, 16).ok())
        })
        .collect()
}

/// `(year, month, day)` of days since the unix epoch (proleptic Gregorian).
#[must_use]
pub const fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    (y, m as u32, d as u32)
}

/// Days since the unix epoch of a civil date.
#[must_use]
pub const fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let m = m as i64;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// `YYYY-MM-DD` of days since the unix epoch.
#[must_use]
pub fn date(days: i64) -> String {
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}")
}

/// The Monday (days since the epoch) of the ISO week holding `days`.
#[must_use]
pub const fn iso_monday(days: i64) -> i64 {
    // 1970-01-01 was a Thursday: weekday 3 counting from Monday = 0.
    days - (days + 3).rem_euclid(7)
}

/// The ISO week of the week starting on `monday`, as `YYYY-Www`.
#[must_use]
pub fn iso_week(monday: i64) -> String {
    let thursday = monday + 3;
    let (year, _, _) = civil_from_days(thursday);
    let week = (thursday - days_from_civil(year, 1, 1)) / 7 + 1;
    format!("{year:04}-W{week:02}")
}

/// Days since the epoch of a time in milliseconds.
#[must_use]
pub fn day_of_ms(ms: u64) -> i64 {
    i64::try_from(ms / DAY_MS).unwrap_or(i64::MAX)
}

/// One row of the queue: a case with its reports counted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaseSummary {
    /// The case.
    pub case: CaseRow,
    /// Reports per reason byte.
    pub reasons: BTreeMap<u8, u64>,
    /// Every report of the target.
    pub reports: u64,
    /// Of those, not decided yet.
    pub open_reports: u64,
    /// Of those, with evidence.
    pub with_evidence: u64,
    /// Of those, with a message checked as written by the target.
    pub checked: u64,
    /// The first day a report arrived.
    pub first_day: u32,
    /// The last day a report arrived.
    pub last_day: u32,
}

/// Filters of the queue.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CaseFilter {
    /// Only this status.
    pub status: Option<CaseStatus>,
    /// Only cases with a report of this reason byte.
    pub reason: Option<u8>,
    /// Only this target-kind byte.
    pub kind: Option<u8>,
    /// Only cases with (`true`) or without (`false`) checked messages.
    pub checked: Option<bool>,
}

/// Fold per-reason aggregates into cases, filter, and order them: cases
/// with checked messages first, then by the number of reports, then the
/// most recent, then by target.
#[must_use]
pub fn fold_cases(aggs: Vec<CaseReasonAgg>, filter: &CaseFilter) -> Vec<CaseSummary> {
    let mut by_case: BTreeMap<(u8, Vec<u8>), CaseSummary> = BTreeMap::new();
    for a in aggs {
        let key = (a.case.target_kind, a.case.target.clone());
        let s = by_case.entry(key).or_insert_with(|| CaseSummary {
            case: a.case.clone(),
            reasons: BTreeMap::new(),
            reports: 0,
            open_reports: 0,
            with_evidence: 0,
            checked: 0,
            first_day: a.first_day,
            last_day: a.last_day,
        });
        *s.reasons.entry(a.reason).or_default() += a.reports;
        s.reports += a.reports;
        s.open_reports += a.open_reports;
        s.with_evidence += a.with_evidence;
        s.checked += a.checked;
        s.first_day = s.first_day.min(a.first_day);
        s.last_day = s.last_day.max(a.last_day);
    }
    let mut out: Vec<CaseSummary> = by_case
        .into_values()
        .filter(|s| filter.status.map_or(true, |st| s.case.status == st))
        .filter(|s| filter.reason.map_or(true, |r| s.reasons.contains_key(&r)))
        .filter(|s| filter.kind.map_or(true, |k| s.case.target_kind == k))
        .filter(|s| filter.checked.map_or(true, |c| (s.checked > 0) == c))
        .collect();
    out.sort_by(|a, b| {
        (b.checked > 0)
            .cmp(&(a.checked > 0))
            .then(b.reports.cmp(&a.reports))
            .then(b.last_day.cmp(&a.last_day))
            .then((a.case.target_kind, &a.case.target).cmp(&(b.case.target_kind, &b.case.target)))
    });
    out
}

/// The name of moderator `id` in `list`, if listed.
#[must_use]
pub fn moderator_name(list: &ModeratorList, id: &[u8; 16]) -> Option<String> {
    list.entries
        .iter()
        .find(|e| &e.moderator_id == id)
        .map(|e| e.name.clone())
}

/// The JSON of a lock, with the holder's name.
#[must_use]
pub fn lock_json(list: &ModeratorList, lock: Option<&CaseLock>) -> Value {
    lock.map_or(Value::Null, |l| {
        json!({
            "moderator_id_b64": B64.encode(l.moderator_id),
            "name": moderator_name(list, &l.moderator_id),
            "at_ms": l.at_ms,
        })
    })
}

/// The JSON of a case.
#[must_use]
pub fn case_json(list: &ModeratorList, case: &CaseRow) -> Value {
    json!({
        "kind": kind_byte_name(case.target_kind),
        "target_b64": B64.encode(&case.target),
        "target_hex": hex(&case.target),
        "status": case.status.name(),
        "lock": lock_json(list, case.lock.as_ref()),
        "decided_at_ms": case.decided_at_ms,
    })
}

/// The JSON of a queue row.
#[must_use]
pub fn summary_json(list: &ModeratorList, s: &CaseSummary) -> Value {
    let reasons: serde_json::Map<String, Value> = s
        .reasons
        .iter()
        .map(|(r, n)| (reason_byte_name(*r).to_owned(), json!(n)))
        .collect();
    let mut v = case_json(list, &s.case);
    v["reasons"] = Value::Object(reasons);
    v["reports"] = json!(s.reports);
    v["open_reports"] = json!(s.open_reports);
    v["with_messages"] = json!(s.with_evidence);
    v["checked_messages_reports"] = json!(s.checked);
    v["first_day"] = json!(date(i64::from(s.first_day)));
    v["last_day"] = json!(date(i64::from(s.last_day)));
    v
}

/// A statement decoded from a log leaf.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoggedStatement {
    /// Its index in the log.
    pub log_index: u64,
    /// The statement.
    pub statement: Statement,
}

/// Decode a statement leaf (`kind ‖ signed statement`); `None` for any other
/// leaf or one that does not decode.
#[must_use]
pub fn statement_of_leaf(log_index: u64, leaf: &[u8]) -> Option<LoggedStatement> {
    let (&kind, rest) = leaf.split_first()?;
    if kind != LEAF_STATEMENT {
        return None;
    }
    let signed = SignedStatement::decode(rest).ok()?;
    let statement = Statement::decode(&signed.statement).ok()?;
    Some(LoggedStatement {
        log_index,
        statement,
    })
}

/// The JSON of a logged statement.
#[must_use]
pub fn statement_json(list: &ModeratorList, s: &LoggedStatement) -> Value {
    let st = &s.statement;
    json!({
        "log_index": s.log_index,
        "statement_id_b64": B64.encode(st.statement_id),
        "action": st.action.name(),
        "target_kind": kind_name(st.target.kind()),
        "target_b64": B64.encode(st.target.bytes()),
        "target_hex": hex(st.target.bytes()),
        "until_ms": (st.until_ms != 0).then_some(st.until_ms),
        "issued_at_ms": st.issued_at_ms,
        "moderator_id_b64": B64.encode(st.moderator_id),
        "moderator_name": moderator_name(list, &st.moderator_id),
        "list_version": st.list_version,
        "reason_commitment_b64": B64.encode(st.reason_commitment),
    })
}

/// The JSON of any log leaf.
#[must_use]
pub fn leaf_json(list: &ModeratorList, index: u64, leaf: &[u8]) -> Value {
    if let Some(s) = statement_of_leaf(index, leaf) {
        return json!({ "index": index, "kind": "statement", "statement": statement_json(list, &s) });
    }
    match leaf.split_first() {
        Some((&LEAF_MODERATOR_LIST, rest)) => json!({
            "index": index,
            "kind": "moderator-list",
            "list_version": ModeratorList::decode_unverified(rest).ok().map(|l| l.list_version),
        }),
        Some((&LEAF_SNAPSHOT_ROOT, _)) => json!({ "index": index, "kind": "snapshot-root" }),
        _ => json!({ "index": index, "kind": "unknown" }),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn dates_and_iso_weeks() {
        assert_eq!(date(0), "1970-01-01");
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        for d in [-1000_i64, 0, 59, 365, 11_016, 20_000, 20_777] {
            let (y, m, dd) = civil_from_days(d);
            assert_eq!(days_from_civil(y, m, dd), d);
        }
        // 2026-11-20 is a Friday in ISO week 47; 2021-01-03 is in 2020-W53;
        // 2024-12-30 is in 2025-W01.
        let d = days_from_civil(2026, 11, 20);
        assert_eq!(date(iso_monday(d)), "2026-11-16");
        assert_eq!(iso_week(iso_monday(d)), "2026-W47");
        assert_eq!(
            iso_week(iso_monday(days_from_civil(2021, 1, 3))),
            "2020-W53"
        );
        assert_eq!(
            iso_week(iso_monday(days_from_civil(2024, 12, 30))),
            "2025-W01"
        );
        assert_eq!(
            iso_week(iso_monday(days_from_civil(1970, 1, 1))),
            "1970-W01"
        );
    }

    #[test]
    fn names_and_hex_round_trip() {
        for k in [
            TargetKind::Account,
            TargetKind::GroupHandle,
            TargetKind::Channel,
            TargetKind::Name,
            TargetKind::Blob,
        ] {
            assert_eq!(kind_from_name(kind_name(k)), Some(k));
        }
        for r in Reason::ALL {
            assert_eq!(reason_from_name(r.name()), Some(r));
        }
        assert_eq!(
            unhex(&hex(&[0, 1, 0xab, 0xff])).unwrap(),
            vec![0, 1, 0xab, 0xff]
        );
        assert!(unhex("abc").is_none());
        assert!(unhex("zz").is_none());
    }

    fn agg(target: u8, reason: u8, reports: u64, checked: u64, day: u32) -> CaseReasonAgg {
        CaseReasonAgg {
            case: CaseRow {
                target_kind: 1,
                target: vec![target; 32],
                status: CaseStatus::New,
                lock: None,
                decided_at_ms: None,
            },
            reason,
            reports,
            open_reports: reports,
            with_evidence: reports,
            checked,
            first_day: day,
            last_day: day,
        }
    }

    #[test]
    fn the_queue_puts_checked_cases_first_then_by_count() {
        let rows = fold_cases(
            vec![
                agg(1, 1, 5, 0, 10),
                agg(2, 1, 1, 1, 11),
                agg(3, 2, 2, 0, 12),
                agg(3, 1, 4, 0, 9),
            ],
            &CaseFilter::default(),
        );
        let order: Vec<u8> = rows.iter().map(|r| r.case.target[0]).collect();
        assert_eq!(order, vec![2, 3, 1]);
        assert_eq!(rows[1].reports, 6);
        assert_eq!(rows[1].first_day, 9);
        assert_eq!(rows[1].last_day, 12);
        let only_harassment = fold_cases(
            vec![agg(1, 1, 5, 0, 10), agg(3, 2, 2, 0, 12)],
            &CaseFilter {
                reason: Some(2),
                ..CaseFilter::default()
            },
        );
        assert_eq!(only_harassment.len(), 1);
        let unchecked = fold_cases(
            vec![agg(1, 1, 5, 0, 10), agg(2, 1, 1, 1, 11)],
            &CaseFilter {
                checked: Some(false),
                ..CaseFilter::default()
            },
        );
        assert_eq!(unchecked.len(), 1);
        assert_eq!(unchecked[0].case.target[0], 1);
    }
}
