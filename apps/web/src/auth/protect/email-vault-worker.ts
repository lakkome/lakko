// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Email-vault worker: runs one seal or open (64 MiB Argon2id, ADR-0067
 * section 2) off the main thread. Companion to `./email-vault.ts`, which
 * owns the worker's lifetime and the main-thread fallback.
 *
 * The recovery phrase crosses the message boundary here (in for a seal, out
 * for an open). Both ends are this app's own code in one origin, and the
 * worker is terminated when the job is answered.
 */
import { loadWasm } from "../../wasm";
import {
  runEmailVaultJob,
  type EmailVaultRequest,
  type EmailVaultResult,
} from "./email-vault-job";

export interface EmailVaultWorkerRequest {
  readonly id: number;
  readonly job: EmailVaultRequest;
}

export type EmailVaultWorkerResponse =
  | { readonly id: number; readonly kind: "result"; readonly result: EmailVaultResult }
  /** The wasm core could not be loaded in this worker: the caller falls
   *  back to the main thread. */
  | { readonly id: number; readonly kind: "unavailable" };

const scope = globalThis as unknown as {
  onmessage: ((ev: MessageEvent<EmailVaultWorkerRequest>) => void) | null;
  postMessage: (message: EmailVaultWorkerResponse) => void;
  WorkerGlobalScope?: unknown;
};

if (typeof scope.WorkerGlobalScope !== "undefined") {
  scope.onmessage = (ev: MessageEvent<EmailVaultWorkerRequest>) => {
    const { id, job } = ev.data;
    void (async (): Promise<void> => {
      let wasm: Awaited<ReturnType<typeof loadWasm>>;
      try {
        wasm = await loadWasm();
      } catch {
        scope.postMessage({ id, kind: "unavailable" });
        return;
      }
      scope.postMessage({ id, kind: "result", result: runEmailVaultJob(wasm, job) });
    })();
  };
}
