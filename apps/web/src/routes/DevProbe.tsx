// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Developer probe — the original 1.16a "Hello core" component.
 *
 * Confirms that:
 *
 *   1. The wasm-pack output loads in this browser.
 *   2. The Rust core can produce an `Identity` and a `KeyPackage`
 *      from inside the page.
 *   3. The MLS one-member group round-trip works (`Group::new`,
 *      `Group::memberCount`).
 *
 * This route is hidden from regular flows and reachable only via
 * `/dev`. Real users will never see it; it stays as a smoke
 * surface that future sub-sprints can use to verify the WASM
 * surface end-to-end without going through the auth / chat UI.
 */
import { Show, createResource, type JSX } from "solid-js";
import { loadWasm } from "../wasm";

/** Format raw bytes as `aabbccdd…` — first 8 bytes is plenty for a UI fingerprint. */
function shortHex(bytes: Uint8Array): string {
  const head = Array.from(bytes.slice(0, 8))
    .map((b) => b.toString(16).padStart(2, "0"))
    .join("");
  return `${head}…`;
}

interface CoreSnapshot {
  signaturePublicKey: Uint8Array;
  keyPackage: Uint8Array;
  membersAtCreate: number;
}

/** Run the Rust core through a single round-trip to confirm wiring. */
async function probeCore(): Promise<CoreSnapshot> {
  const wasm = await loadWasm();

  // 1. Spin up an identity. The label is purely cosmetic.
  const id = new wasm.Identity("web-skeleton");

  // 2. Drive a one-member MLS group through the same surface
  //    Swift/Kotlin clients use — confirms identity → group →
  //    key-package round-trips work in the browser too.
  const group = new wasm.Group(id, wasm.GroupKind.Group);

  return {
    signaturePublicKey: id.signaturePublicKey,
    keyPackage: id.createKeyPackage(),
    membersAtCreate: group.memberCount,
  };
}

export default function DevProbe(): JSX.Element {
  const [core] = createResource(probeCore);

  return (
    <article>
      <h1>Dev probe</h1>
      <p>
        <strong>Status:</strong> verifying the JS&nbsp;↔&nbsp;WASM bridge.
      </p>

      <Show when={!core.loading} fallback={<p>Loading WASM core…</p>}>
        <Show
          when={core.error === undefined}
          fallback={
            <pre style={{ color: "var(--error)", "white-space": "pre-wrap" }}>
              {`error loading core: ${String(core.error)}`}
            </pre>
          }
        >
          {(() => {
            const snap = core();
            if (snap === undefined) return null;
            return (
              <section>
                <h2>Core probe</h2>
                <dl>
                  <dt>Identity public key (first 8 bytes)</dt>
                  <dd>
                    <code>{shortHex(snap.signaturePublicKey)}</code>
                  </dd>
                  <dt>KeyPackage size</dt>
                  <dd>
                    <code>{snap.keyPackage.length} bytes</code>
                  </dd>
                  <dt>Group member count after create</dt>
                  <dd>
                    <code>{snap.membersAtCreate}</code>
                  </dd>
                </dl>
                <p>
                  WASM core is alive. 1.16c (auth) wires the OPAQUE-PAKE
                  flow on top of this same loader.
                </p>
              </section>
            );
          })()}
        </Show>
      </Show>
    </article>
  );
}
