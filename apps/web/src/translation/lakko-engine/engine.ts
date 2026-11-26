// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0100 — Lakko's own on-device translation engine, last in the engine
 * order of ADR-0082 (`device-engines.ts`): it owns a pair only where no
 * engine before it (the desktop host's, the browser's `Translator`) does.
 *
 * Languages come in groups of three (section 4); a pair is `ready` when
 * every group its route needs is on this device, `needs-pack` otherwise, and
 * `unavailable` where the manifest cannot make it or this browser cannot run
 * the engine. A group is downloaded only inside `translate()`, which the
 * controller calls after the person agreed (UX 3.5).
 *
 * Everything that touches the network or storage arrives through
 * {@link LakkoEngineDeps}; this module itself reaches no network
 * (`../fail-closed.test.ts`).
 */

import type { EngineState, OwnPack, PackDisclosure, ProgressListener, TranslationEngine } from "../engines";
import { groupsFor, type Manifest, type ModelFileKind, routeFor } from "./manifest";
import type { WorkerModelFile, WorkerRequest } from "./protocol";

/** A model file's place in its group file. */
export interface Slice {
  readonly offset: number;
  readonly length: number;
  readonly sha256: string;
}

/** One worker, as the engine talks to it. */
export interface EnginePort {
  call(request: DistributiveOmit<WorkerRequest, "id">, transfer?: Transferable[]): Promise<string | null>;
  /** Stop the worker and free its memory. */
  terminate(): void;
}

type DistributiveOmit<T, K extends PropertyKey> = T extends unknown ? Omit<T, K> : never;

export interface LakkoEngineDeps {
  readonly manifest: Manifest | null;
  /** This browser can run the engine, and this client offers it here (not a
   *  phone browser: ADR-0100 open question 4). */
  readonly supported: boolean;
  storedGroups(): Promise<readonly string[]>;
  readSlice(group: string, offset: number, length: number): Promise<ArrayBuffer | null>;
  downloadGroup(
    group: string,
    bytes: number,
    slices: readonly Slice[],
    options: { readonly onProgress?: ProgressListener; readonly signal?: AbortSignal },
  ): Promise<void>;
  removeGroup(group: string): Promise<void>;
  engineBinary(): Promise<ArrayBuffer>;
  /** A new worker. Each direction runs in a worker of its own: one engine
   *  instance holding two models translated wrongly in Safari (empty or cut
   *  output from the model loaded second, seen 2026-11-25 on macOS 27.2;
   *  Firefox and Opera were not affected), and separate instances isolate
   *  them in every browser. */
  newPort(): EnginePort;
}

/** How many directions stay loaded, one worker each; a route through
 *  English needs two. Chosen, not measured. */
export const LOADED_DIRECTIONS_MAX = 2;

const KIND_ORDER: readonly ModelFileKind[] = ["model", "lex", "vocab", "srcvocab", "trgvocab"];

export function lakkoTranslation(deps: LakkoEngineDeps): TranslationEngine {
  const manifest = deps.manifest;
  if (manifest === null || !deps.supported) {
    return {
      present: false,
      state: () => Promise.resolve("unavailable"),
      translate: () => Promise.reject(new Error("no on-device translator")),
    };
  }

  let stored: Set<string> | null = null;
  const storedNow = async (): Promise<Set<string>> => {
    if (stored === null) stored = new Set(await deps.storedGroups().catch(() => []));
    return stored;
  };
  const downloads = new Map<string, Promise<void>>();
  let binary: Promise<ArrayBuffer> | null = null;
  /** Loaded directions, least recently used first, each with its worker. */
  const loaded: { key: string; port: EnginePort }[] = [];
  let chain: Promise<unknown> = Promise.resolve();
  /** One worker operation at a time, in order. */
  const serial = <T>(run: () => Promise<T>): Promise<T> => {
    const next = chain.then(run, run);
    chain = next.catch(() => undefined);
    return next;
  };

  const slicesOf = (fileHash: string): Slice[] => {
    const group = manifest.groups.find((g) => g.file === fileHash);
    if (group === undefined) return [];
    return Object.values(manifest.directions)
      .filter((d) => d.group === group.id)
      .flatMap((d) => Object.values(d.files))
      .map((f) => ({ offset: f.offset, length: f.length, sha256: f.sha256 }));
  };

  const ensureGroups = async (route: readonly string[], onProgress?: ProgressListener, signal?: AbortSignal): Promise<void> => {
    const have = await storedNow();
    const missing = groupsFor(manifest, route).filter((g) => !have.has(g.file));
    const total = missing.reduce((n, g) => n + g.bytes, 0);
    let done = 0;
    for (const g of missing) {
      let pending = downloads.get(g.file);
      if (pending === undefined) {
        const base = done;
        pending = deps
          .downloadGroup(g.file, g.bytes, slicesOf(g.file), {
            onProgress: (f) => onProgress?.((base + f * g.bytes) / total),
            ...(signal !== undefined ? { signal } : {}),
          })
          .then(() => {
            have.add(g.file);
          })
          .finally(() => downloads.delete(g.file));
        downloads.set(g.file, pending);
      }
      await pending;
      done += g.bytes;
    }
  };

  const engineBinary = (): Promise<ArrayBuffer> => {
    if (binary === null) {
      binary = deps.engineBinary();
      binary.catch(() => {
        binary = null;
      });
    }
    return binary;
  };

  const drop = (key: string): void => {
    const at = loaded.findIndex((l) => l.key === key);
    if (at < 0) return;
    loaded[at]?.port.terminate();
    loaded.splice(at, 1);
  };

  /** The worker holding `key`, loading it in a new worker when absent. */
  const ensureLoaded = async (key: string): Promise<EnginePort> => {
    const at = loaded.findIndex((l) => l.key === key);
    if (at >= 0) {
      const [entry] = loaded.splice(at, 1);
      if (entry !== undefined) {
        loaded.push(entry);
        return entry.port;
      }
    }
    const d = manifest.directions[key];
    const group = manifest.groups.find((g) => g.id === d?.group);
    if (d === undefined || group === undefined) throw new Error("unknown direction");
    const files: WorkerModelFile[] = [];
    for (const kind of KIND_ORDER) {
      const f = d.files[kind];
      if (f === undefined) continue;
      const zstd = await deps.readSlice(group.file, f.offset, f.length);
      if (zstd === null) throw new Error("model group missing");
      files.push({ kind, name: f.name, zstd, decompressedSha256: f.decompressedSha256, decompressedBytes: f.decompressedBytes });
    }
    // Make room first, so two engines' memory is the most ever held.
    while (loaded.length >= LOADED_DIRECTIONS_MAX) drop(loaded[0]?.key ?? "");
    const wasm = (await engineBinary()).slice(0);
    const port = deps.newPort();
    try {
      await port.call({ type: "init", wasm }, [wasm]);
      const { source, target } = directionLanguages(key);
      await port.call({ type: "load", key, source, target, files }, files.map((f) => f.zstd));
    } catch (err) {
      port.terminate();
      throw err;
    }
    loaded.push({ key, port });
    return port;
  };

  return {
    present: true,
    fetchesPacks: true,
    async state(source, target): Promise<EngineState> {
      const route = routeFor(manifest, source, target);
      if (route.length === 0) return "unavailable";
      const have = await storedNow();
      return groupsFor(manifest, route).every((g) => have.has(g.file)) ? "ready" : "needs-pack";
    },
    async translate(source, target, text, options = {}): Promise<string> {
      const route = routeFor(manifest, source, target);
      if (route.length === 0) throw new Error("no on-device translator for this pair");
      await ensureGroups(route, options.onProgress, options.signal);
      options.signal?.throwIfAborted();
      return serial(async () => {
        // Through English one direction after the other, as Bergamot's own
        // pivoting does; a route never needs more than two loaded.
        const ports: EnginePort[] = [];
        for (const key of route) ports.push(await ensureLoaded(key));
        let out = text;
        for (const port of ports) out = (await port.call({ type: "translate", text: out })) ?? "";
        return out;
      });
    },
    packFetch: () => "engine",
    async packDisclosure(source, target): Promise<PackDisclosure | null> {
      const route = routeFor(manifest, source, target);
      if (route.length === 0) return null;
      const have = await storedNow();
      const missing = groupsFor(manifest, route).filter((g) => !have.has(g.file));
      return { groups: missing.map((g) => ({ languages: g.languages, bytes: g.bytes })) };
    },
    async ownPacks(): Promise<OwnPack[]> {
      const have = await storedNow();
      return manifest.groups
        .filter((g) => have.has(g.file))
        .map((g) => ({ id: g.file, languages: g.languages, bytes: g.bytes }));
    },
    async removeOwnPack(id): Promise<void> {
      await deps.removeGroup(id);
      (await storedNow()).delete(id);
      const group = manifest.groups.find((g) => g.file === id);
      if (group === undefined) return;
      await serial(async () => {
        for (const l of [...loaded]) {
          if (manifest.directions[l.key]?.group === group.id) drop(l.key);
        }
      });
    },
  };
}

/** The source and target of a direction key (`es-en`, `en-zh-Hans`). */
export function directionLanguages(key: string): { source: string; target: string } {
  if (key.startsWith("en-")) return { source: "en", target: key.slice(3) };
  if (key.endsWith("-en")) return { source: key.slice(0, -3), target: "en" };
  const i = key.indexOf("-");
  return { source: key.slice(0, i), target: key.slice(i + 1) };
}
