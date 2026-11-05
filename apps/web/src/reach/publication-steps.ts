// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The steps of one publication pass run one after another, each on its own
 * (base walk 2026-11-04, `coc-discovery-findings.md` item 1): rebuilding the
 * device's own introduction record needs an OPRF round (a `discovery-register`
 * token, the network), and when that threw, the relay of the CONTACTS'
 * records never ran, so a device that could not rebuild its own record also
 * stopped carrying everyone else's. iOS already relays after its own record
 * fails. A failed step is logged and the next one runs; the pass then reports
 * the failure so it is tried again.
 */

export interface PublicationStep {
  readonly name: string;
  readonly run: () => Promise<void>;
}

/** Thrown at the end of a pass when a step failed. */
export class PublicationStepsFailed extends Error {
  constructor(readonly failed: readonly string[]) {
    super(`publication steps failed: ${failed.join(", ")}`);
    this.name = "PublicationStepsFailed";
  }
}

/** Run every step in order; a failure does not stop the next one. Rejects
 *  with {@link PublicationStepsFailed} naming the failed steps, after all
 *  have run. */
export async function runPublicationSteps(
  steps: readonly PublicationStep[],
  onFailure: (name: string, err: unknown) => void = () => {},
): Promise<void> {
  const failed: string[] = [];
  for (const step of steps) {
    try {
      await step.run();
    } catch (err) {
      failed.push(step.name);
      onFailure(step.name, err);
    }
  }
  if (failed.length > 0) throw new PublicationStepsFailed(failed);
}
