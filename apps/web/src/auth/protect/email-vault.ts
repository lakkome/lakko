// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Seal and open the email vault (ADR-0075 section 2) without blocking the
 * page: each job runs in its own module worker (`email-vault-worker.ts`),
 * which is terminated when it answers. Where a worker cannot be started or
 * cannot load the wasm core, the job runs on the main thread after one
 * yield, so the progress line the caller put up is painted first.
 */
import { loadWasm } from "../../wasm";
import {
  runEmailVaultJob,
  type EmailVaultCore,
  type EmailVaultRequest,
  type EmailVaultResult,
} from "./email-vault-job";
import type { EmailVaultWorkerRequest, EmailVaultWorkerResponse } from "./email-vault-worker";

/** The worker as this module uses it (injectable in tests). */
export interface EmailVaultWorkerLike {
  onmessage: ((ev: { data: EmailVaultWorkerResponse }) => void) | null;
  onerror: ((ev: unknown) => void) | null;
  postMessage(message: EmailVaultWorkerRequest): void;
  terminate(): void;
}

let workerFactory: (() => EmailVaultWorkerLike) | null =
  typeof Worker === "undefined"
    ? null
    : () =>
        new Worker(new URL("./email-vault-worker.ts", import.meta.url), {
          type: "module",
          name: "lakko-email-vault",
        }) as unknown as EmailVaultWorkerLike;
let coreLoader: () => Promise<EmailVaultCore> = () => loadWasm();
let nextId = 1;

/** Tests: replace the worker factory (`null`: no worker) and the core. */
export function setEmailVaultRuntimeForTests(runtime: {
  readonly workerFactory: (() => EmailVaultWorkerLike) | null;
  readonly core: () => Promise<EmailVaultCore>;
}): void {
  workerFactory = runtime.workerFactory;
  coreLoader = runtime.core;
}

function inWorker(factory: () => EmailVaultWorkerLike, job: EmailVaultRequest): Promise<EmailVaultResult | null> {
  return new Promise((resolve) => {
    let worker: EmailVaultWorkerLike;
    try {
      worker = factory();
    } catch {
      resolve(null);
      return;
    }
    const id = nextId;
    nextId += 1;
    const done = (result: EmailVaultResult | null): void => {
      worker.terminate();
      resolve(result);
    };
    worker.onmessage = (ev) => {
      if (ev.data.id !== id) return;
      done(ev.data.kind === "result" ? ev.data.result : null);
    };
    worker.onerror = () => done(null);
    worker.postMessage({ id, job });
  });
}

/** Run one job, in a worker when one can be had. */
export async function runEmailVault(job: EmailVaultRequest): Promise<EmailVaultResult> {
  if (workerFactory !== null) {
    const result = await inWorker(workerFactory, job);
    if (result !== null) return result;
  }
  // No worker: yield once so the caller's progress line is painted before
  // the page is busy.
  await new Promise<void>((resolve) => setTimeout(resolve, 0));
  return runEmailVaultJob(await coreLoader(), job);
}
