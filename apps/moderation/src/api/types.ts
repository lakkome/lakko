// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/** The JSON shapes of the moderation service's panel API, as built. */

export interface SessionStart {
  readonly challenge_b64: string;
  readonly expires_at_ms: number;
  readonly rp_id: string;
  readonly origin: string;
  readonly credentials: readonly { readonly moderator_id_b64: string; readonly credential_id_b64: string }[];
}

export interface SessionFinish {
  readonly token: string;
  readonly expires_at_ms: number;
  readonly moderator_id_b64: string;
  readonly name: string;
}

export interface Me {
  readonly moderator_id_b64: string;
  readonly name: string;
  readonly expires_at_ms: number;
}

export interface ModeratorRow {
  readonly moderator_id_b64: string;
  readonly name: string;
  readonly key_kind: string;
  readonly credential_id_b64: string;
  readonly x25519_public_b64: string;
  readonly added_at_ms: number;
  readonly revoked_at_ms: number | null;
}

export interface Moderators {
  readonly list_version: number;
  readonly issued_at_ms: number;
  readonly rp_id: string;
  readonly origin: string;
  readonly list_b64: string;
  readonly moderators: readonly ModeratorRow[];
}

export interface Lock {
  readonly moderator_id_b64: string;
  readonly name: string;
  readonly at_ms: number;
}

export interface CaseRow {
  readonly kind: string;
  readonly target_b64: string;
  readonly target_hex: string;
  readonly status: string;
  readonly lock: Lock | null;
  readonly decided_at_ms: number | null;
  readonly reasons: Readonly<Record<string, number>>;
  readonly reports: number;
  readonly open_reports: number;
  readonly with_messages: number;
  readonly checked_messages_reports: number;
  readonly first_day: string;
  readonly last_day: string;
}

export interface Cases {
  readonly cases: readonly CaseRow[];
  readonly total: number;
}

export interface CaseHead {
  readonly kind: string;
  readonly target_b64: string;
  readonly target_hex: string;
  readonly status: string;
  readonly lock: Lock | null;
  readonly decided_at_ms: number | null;
}

export interface Checks {
  readonly checked: number;
  readonly unchecked: number;
}

export interface ReportRow {
  readonly receipt_id_b64: string;
  readonly receipt_hex: string;
  readonly reason: string;
  readonly reason_code: number;
  readonly conversation_kind: string;
  readonly evidence_count: number;
  readonly list_version: number;
  readonly day: string;
  readonly status: string;
  readonly report_b64: string;
  readonly checks: Checks | null;
}

export interface Statement {
  readonly log_index: number;
  readonly statement_id_b64: string;
  readonly action: string;
  readonly target_kind: string;
  readonly target_b64: string;
  readonly target_hex: string;
  readonly until_ms: number | null;
  readonly issued_at_ms: number;
  readonly moderator_id_b64: string;
  readonly moderator_name: string | null;
  readonly list_version: number;
  readonly reason_commitment_b64: string;
}

export type HistoryEntry =
  | {
      readonly type: "statement";
      readonly at_ms: number;
      readonly statement: Statement;
      readonly reason: string | null;
      readonly note: { readonly text: string; readonly moderator_name: string; readonly at_ms: number } | null;
    }
  | {
      readonly type: "no-action";
      readonly at_ms: number;
      readonly reason: string;
      readonly moderator_id_b64: string;
      readonly moderator_name: string;
      readonly note: { readonly text: string; readonly moderator_name: string; readonly at_ms: number } | null;
    };

export interface AuthRead {
  readonly exists: boolean;
  readonly age_band: string | null;
  readonly suspended: boolean;
  readonly until_ms: number | null;
}

export interface DiscoveryRead {
  readonly has_username: boolean;
  readonly in_everyone: boolean;
  readonly name_lookup_keys_b64: readonly string[];
}

export interface CaseDetail {
  readonly case: CaseHead;
  readonly reports: readonly ReportRow[];
  readonly history: readonly HistoryEntry[];
  readonly account: { readonly auth: AuthRead | null; readonly discovery: DiscoveryRead | null } | null;
}

export interface DecideAnswer {
  readonly log_index: number;
  readonly statement_id_b64: string;
  readonly reports_closed: number;
  readonly forwarded?: boolean;
  readonly applied?: boolean;
  readonly repeat?: boolean;
  readonly superseded?: boolean;
  readonly already_removed?: boolean;
  /** A kept suspension had no appeal waiting (`keep-suspension`). */
  readonly no_pending_appeal?: boolean;
  readonly error?: string;
  readonly detail?: string | null;
}

export interface Appeal {
  readonly target_b64: string;
  readonly target_hex: string;
  readonly sealed_b64: string;
  readonly statement_id_b64: string;
  readonly log_index: number | null;
  readonly until_ms: number | null;
  readonly appeal_at_ms: number;
  readonly decision: Statement | null;
  readonly decided_by_you: boolean;
  readonly case: CaseHead | null;
}

export interface Appeals {
  readonly available: boolean;
  readonly appeals: readonly Appeal[];
}

export type LogEntry =
  | { readonly index: number; readonly kind: "statement"; readonly statement: Statement }
  | { readonly index: number; readonly kind: "moderator-list"; readonly list_version: number | null }
  | { readonly index: number; readonly kind: "snapshot-root" }
  | { readonly index: number; readonly kind: "unknown" };

export interface LogPage {
  readonly log_size: number;
  readonly next_before: number | null;
  readonly entries: readonly LogEntry[];
}

export interface NumbersWeek {
  readonly week: string;
  readonly start_day: string;
  readonly reports: Readonly<Record<string, number>>;
  readonly decisions: Readonly<Record<string, number>>;
  readonly appeals: { readonly sent: number; readonly declined: number; readonly lifted: number };
}

export interface Numbers {
  readonly weeks: readonly NumbersWeek[];
}
