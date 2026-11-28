// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { afterEach, describe, expect, it } from "vitest";

import {
  pendingConfirm,
  resolvePendingConfirm,
  setConfirmHostMounted,
} from "../../ui/confirm-dialog";
import type { AccountFactors } from "./factors";
import { lastFactorNotice, refusedAsLastFactor } from "./last-factor";

const read = (rel: string): string => readFileSync(fileURLToPath(new URL(rel, import.meta.url)), "utf8");
const f = (over: Partial<AccountFactors> = {}): AccountFactors => ({
  email: false,
  totp: false,
  passkeys: 0,
  mailAvailable: true,
  ...over,
});

afterEach(() => setConfirmHostMounted(false));

describe("the last-factor dialog (UX 19.1c)", () => {
  it("is the app's own notice: the one line and OK", () => {
    const email = lastFactorNotice("email");
    expect(email.body).toBe("Add another way to get back in first. This is the last one this account has.");
    expect(email.confirmLabel).toBe("OK");
    expect(email.notice).toBe(true);
    expect(email.tone).toBe("neutral");
    expect(email.title).toBe("Recovery email");
    expect(lastFactorNotice("passkey").title).toBe("Passkeys");
  });

  it("refuses the removal of the last one before anything is sent, and shows the dialog", async () => {
    setConfirmHostMounted(true);
    const refused = refusedAsLastFactor(f({ email: true }), "email");
    expect(pendingConfirm()?.request.body).toContain("This is the last one");
    resolvePendingConfirm(true);
    expect(await refused).toBe(true);
  });

  it("refuses the last passkey the same way", async () => {
    setConfirmHostMounted(true);
    const refused = refusedAsLastFactor(f({ passkeys: 1 }), "passkey");
    expect(pendingConfirm()).not.toBeNull();
    resolvePendingConfirm(false);
    expect(await refused).toBe(true);
  });

  it("lets a removal through when another way stays, with no dialog", async () => {
    setConfirmHostMounted(true);
    expect(await refusedAsLastFactor(f({ email: true, totp: true }), "email")).toBe(false);
    expect(await refusedAsLastFactor(f({ passkeys: 2 }), "passkey")).toBe(false);
    expect(pendingConfirm()).toBeNull();
  });

  it("unknown facts refuse nothing here: the server decides", async () => {
    setConfirmHostMounted(true);
    expect(await refusedAsLastFactor(null, "email")).toBe(false);
    expect(pendingConfirm()).toBeNull();
  });
});

describe("where the rule is applied", () => {
  it("the email's Remove asks the rule before it opens, on both surfaces", () => {
    expect(read("./ProtectionCard.tsx").includes('if (await refusedAsLastFactor(factors(), "email")) return;')).toBe(
      true,
    );
    expect(read("./AccountNotices.tsx").includes('"email")) return;')).toBe(true);
  });

  it("a server 409 last-factor closes the email removal with the same dialog", () => {
    const remove = read("./RemoveRecoveryEmail.tsx");
    const at = remove.indexOf('if (read.kind === "last-factor") {');
    expect(at).toBeGreaterThan(0);
    expect(remove.indexOf('await showLastFactorNotice("email");', at)).toBeGreaterThan(at);
  });

  it("deleting a passkey asks the rule before the confirmation and the passkey gesture", () => {
    const section = read("../PasskeysSection.tsx");
    const del = section.slice(section.indexOf("async function onDelete("));
    const rule = del.indexOf('if (await refusedAsLastFactor(factors, "passkey")) return;');
    expect(rule).toBeGreaterThan(0);
    expect(rule).toBeLessThan(del.indexOf("await requestConfirm(request)"));
    expect(rule).toBeLessThan(del.indexOf("await c.assertAction(actionHashB64);"));
    // And reads the server's refusal.
    expect(del.includes('await showLastFactorNotice("passkey");')).toBe(true);
  });

  it("a removal that leaves only the authenticator says so in its confirmation", () => {
    expect(read("../PasskeysSection.tsx").includes('removeNoticeKey(factors, "passkey",')).toBe(true);
    expect(read("./RemoveRecoveryEmail.tsx").includes('removeNoticeKey(accountFactors(accountId()), "email",')).toBe(
      true,
    );
  });
});
