// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0073 R.2 with rule 4 (amended 2026-11-26, the second time; ADR-0101
 * section 10) - the acceptance rule for a contact's account-key RESET.
 *
 * The table itself is NOT written here any more. Until 2026-11-26 this
 * module held a hand-written copy of it, as did the iOS and Android
 * clients; it is now defined once, in the core
 * (`lakko_contacts::reset_verdict`), and this module is the thin adapter
 * the web client calls: it runs the seal's signature half
 * (`checkResetSeal`) and hands every check's result to `resetVerdict`.
 *
 * A reset is a key change with NO supersession proof (with one, ADR-0016's
 * path runs and this module is never asked). The outcomes:
 *
 *   - `sealed`: rules 1 to 4 hold. The same person: re-pin, move
 *     everything (the old `accepted`).
 *   - `unsealed`: rules 1 to 3 hold and rule 4 does not. A new account
 *     under the same username (ADR-0101 section 11): no re-pin onto the old
 *     contact, nothing moved, nothing returned.
 *   - `refused`: the fail-closed refusal.
 *   - `no-verdict`: nothing can be decided yet.
 *
 * `contacts/client.ts` gathers the inputs and acts on the outcome.
 */
import { base64Decode } from "../auth/account-id";
import { loadWasm } from "../wasm";

/** What the caller learned by running `verifyKeyResetAttestation` over the
 *  bundle's `key_reset_attest` under the trusted attest key. */
export type AttestCheck =
  /** The bundle carries no attestation. */
  | { readonly kind: "absent" }
  /** It carries one and the verifier refused it (signature, account
   *  binding, key binding), or it is not decodable. */
  | { readonly kind: "invalid" }
  /** No attest key could be obtained (no build-time pin and the JWKS fetch
   *  failed): nothing can be said about the attestation yet. */
  | { readonly kind: "no-key" }
  /** It verified for this account id and the fetched key. */
  | { readonly kind: "verified"; readonly keyGeneration: number; readonly signedAtMs: number };

/** The verified key-log answer for the fetched key (`verifyIdentityInLog`). */
export interface LogCheck {
  readonly status: "confirmed" | "absent" | "rollback" | "mismatch" | "unreachable" | "off";
  /** The log's `is_reset` marker when a latest entry was read. */
  readonly isReset: boolean | null;
}

/**
 * The key-log half of rule 4 (`verifySealAnnouncementInLog`): whether the
 * served previous announcement is in the log before the reset leaf. Ignored
 * by the core when the build has no key log.
 */
export type SealLog =
  /** The served announcement is in the log before the reset leaf. */
  | "proven"
  /** The log was read and does not show it. */
  | "not-proven"
  /** The log could not be reached for the announcement. */
  | "unreachable";

/** Which refusal copy the action shows (`contacts.identity_changed.*`). */
export type ResetRefusal =
  /** The key log shows the new key and does NOT record a reset. */
  | "unexplained"
  /** Everything else that is negative. */
  | "unconfirmed";

export type ResetVerdict =
  | {
      readonly kind: "sealed";
      readonly keyGeneration: number;
      readonly signedAtMs: number;
      /** The previous announcement's `seq`: the account's new floor. */
      readonly announcementSeq: bigint;
    }
  | { readonly kind: "unsealed"; readonly keyGeneration: number; readonly signedAtMs: number }
  | { readonly kind: "refused"; readonly why: ResetRefusal }
  | { readonly kind: "no-verdict" };

/** The facts of one fetched bundle the rule reads. */
export interface ResetVerdictInput {
  /** The peer's raw 16-byte account id. */
  readonly accountId: Uint8Array;
  /** The key this device HOLDS for the account (the pin, or a verified
   *  binding's key for a member it never pinned), standard base64. */
  readonly heldKeyB64: string;
  /** The generation this device last accepted for the account; 0 when none
   *  was recorded. The previous announcement must be of exactly this
   *  generation. */
  readonly acceptedGeneration: number;
  /** The fetched bundle's key and generation. */
  readonly fetchedKeyB64: string;
  readonly bundleGeneration: number;
  readonly attest: AttestCheck;
  readonly log: LogCheck;
  /** The bundle's `previous_reset_seals` (the newest announcement of the
   *  generation before the reset), or `null` when it carries none. */
  readonly previousAnnouncement: Uint8Array | null;
  /** The bundle's `reset_seal`, or `null` when it carries none. */
  readonly resetSeal: Uint8Array | null;
  /** The highest announcement `seq` this device has seen for the account. */
  readonly seqFloor: bigint;
  /** The key-log half of rule 4. */
  readonly sealLog: SealLog;
}

function decodeKey(b64: string): Uint8Array | null {
  try {
    const bytes = base64Decode(b64);
    return bytes.length === 32 ? bytes : null;
  } catch {
    return null;
  }
}

function generation(n: number): bigint | null {
  return Number.isSafeInteger(n) && n >= 0 ? BigInt(n) : null;
}

/**
 * R.2 with rule 4. Never throws: an input the core cannot read (a key of
 * the wrong length, a generation that is not a count) is the fail-closed
 * refusal, exactly as a bundle that fails a check.
 */
export async function resetVerdict(input: ResetVerdictInput): Promise<ResetVerdict> {
  if (input.fetchedKeyB64 === input.heldKeyB64) return { kind: "no-verdict" };
  const held = decodeKey(input.heldKeyB64);
  const fetched = decodeKey(input.fetchedKeyB64);
  const accepted = generation(input.acceptedGeneration);
  const bundleGeneration = generation(input.bundleGeneration);
  if (held === null || fetched === null || accepted === null || input.accountId.length !== 16) {
    return { kind: "refused", why: "unconfirmed" };
  }
  const attest = input.attest;
  const attestGeneration = attest.kind === "verified" ? generation(attest.keyGeneration) : 0n;
  const attestSignedAt =
    attest.kind === "verified" && Number.isSafeInteger(attest.signedAtMs) ? BigInt(attest.signedAtMs) : 0n;
  if (attestGeneration === null) return { kind: "refused", why: "unconfirmed" };
  const wasm = await loadWasm();
  // Rule 4, the signature half: the previous announcement under the held
  // key, its `seq` floor, the reset seal and its announced key.
  let sealValid = false;
  let announcementSeq = 0n;
  try {
    const check = wasm.checkResetSeal(
      input.previousAnnouncement,
      input.resetSeal,
      held,
      input.accountId,
      accepted,
      attestGeneration,
      fetched,
      input.seqFloor,
    );
    try {
      sealValid = check.valid;
      announcementSeq = check.announcementSeq;
    } finally {
      check.free();
    }
  } catch {
    sealValid = false;
  }
  try {
    const verdict = wasm.resetVerdict(
      held,
      accepted,
      fetched,
      // A bundle generation that is not a count can never equal the
      // attested one: `u64::MAX` is refused by rule 3 like any mismatch.
      bundleGeneration ?? 0xffff_ffff_ffff_ffffn,
      attest.kind,
      attestGeneration,
      attestSignedAt,
      input.log.status,
      input.log.isReset,
      sealValid,
      announcementSeq,
      input.sealLog,
    );
    try {
      switch (verdict.kind) {
        case "sealed":
          return {
            kind: "sealed",
            keyGeneration: Number(verdict.keyGeneration ?? 0n),
            signedAtMs: Number(verdict.signedAtMs ?? 0n),
            announcementSeq: verdict.announcementSeq ?? announcementSeq,
          };
        case "unsealed":
          return {
            kind: "unsealed",
            keyGeneration: Number(verdict.keyGeneration ?? 0n),
            signedAtMs: Number(verdict.signedAtMs ?? 0n),
          };
        case "refused":
          return { kind: "refused", why: verdict.why === "unexplained" ? "unexplained" : "unconfirmed" };
        default:
          return { kind: "no-verdict" };
      }
    } finally {
      verdict.free();
    }
  } catch {
    return { kind: "refused", why: "unconfirmed" };
  }
}

/**
 * The rotation marker of the re-pin (R.2 "The re-pin"): never backwards, in
 * ADR-0016's marker space (seconds).
 */
export function resetRepinMarker(heldMarker: number, nowMs: number): number {
  return Math.max(heldMarker, Math.floor(nowMs / 1000));
}
