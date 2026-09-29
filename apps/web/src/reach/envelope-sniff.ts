// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0060 - recognising the scoped-discovery service envelopes in an
 * application plaintext, BEFORE the chat parser (whose lossy-UTF-8 fallback
 * would otherwise render them as a bubble of domain bytes and secrets).
 *
 * Five kinds (IntroductionOffer, Introductions, Visibility, ReachState,
 * CommitterDelegation), plus the ADR-0057 RegistrarSeed the same dispatcher
 * names: each is dispatched to its handler and never rendered. The verdict is
 * the core's `dispatchIncomingEnvelope`; this file copies no domain bytes.
 * `dispatchIncomingEnvelope` falls back to `IntroMessage` for anything it does
 * not recognise, which is exactly "not mine".
 */

/** The service-envelope kinds this sniff owns. */
export type ScopedEnvelopeKind =
  | "introduction-offer"
  | "introductions"
  | "visibility"
  | "reach-state"
  | "committer-delegation"
  | "registrar-seed";

/** The part of the wasm surface the sniff needs, structurally. */
export interface EnvelopeDispatcher {
  dispatchIncomingEnvelope(bytes: Uint8Array): { readonly kind: number; free?(): void };
  readonly IncomingEnvelopeKind: {
    readonly IntroductionOffer: number;
    readonly Introductions: number;
    readonly Visibility: number;
    readonly ReachState: number;
    readonly CommitterDelegation: number;
    readonly RegistrarSeed: number;
  };
}

/** First byte every `lakko/...` service-envelope domain starts with. A cheap
 *  skip for chat envelopes (JSON, `{`); never a verdict. */
const SERVICE_DOMAIN_FIRST_BYTE = 0x6c;

/** Classify a plaintext; `null` when it is none of these kinds. Never throws. */
export function sniffScopedEnvelope(
  wasm: EnvelopeDispatcher,
  bytes: Uint8Array,
): ScopedEnvelopeKind | null {
  if (bytes.length === 0 || bytes[0] !== SERVICE_DOMAIN_FIRST_BYTE) return null;
  let env: { readonly kind: number; free?(): void };
  try {
    env = wasm.dispatchIncomingEnvelope(bytes);
  } catch {
    return null;
  }
  const k = env.kind;
  try {
    env.free?.();
  } catch {
    /* best-effort */
  }
  const K = wasm.IncomingEnvelopeKind;
  if (k === K.IntroductionOffer) return "introduction-offer";
  if (k === K.Introductions) return "introductions";
  if (k === K.Visibility) return "visibility";
  if (k === K.ReachState) return "reach-state";
  if (k === K.CommitterDelegation) return "committer-delegation";
  if (k === K.RegistrarSeed) return "registrar-seed";
  return null;
}
