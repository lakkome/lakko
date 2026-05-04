// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Lakko web client root component (Phase 1.16a skeleton).
 *
 * Goals at this stage are deliberately narrow:
 *
 *   1. Prove the WASM module loads in the browser.
 *   2. Prove the Rust core can produce an `Identity` and a
 *      `KeyPackage` from inside the page.
 *   3. Render the resulting fingerprint so the human running
 *      `npm run dev` can eyeball "yes, this works".
 *
 * Routing, signup / login, contacts, chat all live in 1.16b. This
 * file is the smallest surface that proves the JS↔WASM bridge is
 * functional end-to-end.
 */
import { Show, createResource, type Component } from "solid-js";
import { loadWasm } from "./wasm";

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

  // 1. Spin up an identity. The label is purely cosmetic in 1.16a.
  const id = new wasm.Identity("web-skeleton");

  // 2. Drive a one-member MLS group through the same surface
  //    Swift/Kotlin clients use — confirms identity → group →
  //    key-package round-trips work in the browser too.
  const group = new wasm.Group(id);

  return {
    signaturePublicKey: id.signaturePublicKey,
    keyPackage: id.createKeyPackage(),
    membersAtCreate: group.memberCount,
  };
}

const App: Component = () => {
  // `createResource` re-runs on signal changes; here it fires once.
  const [core] = createResource(probeCore);

  return (
    <main
      style={{
        "font-family": "system-ui, sans-serif",
        "max-width": "640px",
        margin: "2rem auto",
        padding: "0 1rem",
        "line-height": "1.5",
      }}
    >
      <h1>Lakko web client</h1>
      <p>
        <strong>Status:</strong> Phase 1.16a skeleton — verifying the
        JS&nbsp;↔&nbsp;WASM bridge.
      </p>

      <Show
        when={!core.loading}
        fallback={<p>Loading WASM core…</p>}
      >
        <Show
          when={core.error === undefined}
          fallback={
            <pre
              style={{
                color: "#b00020",
                "white-space": "pre-wrap",
              }}
            >
              {`error loading core: ${String(core.error)}`}
            </pre>
          }
        >
          {(() => {
            const snap = core();
            if (!snap) return null;
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
                  WASM core is alive. Phase 1.16b wires up signup,
                  login, contact list, and 1×1 chat.
                </p>
              </section>
            );
          })()}
        </Show>
      </Show>
    </main>
  );
};

export default App;
