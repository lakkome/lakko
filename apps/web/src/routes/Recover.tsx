// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Recover-from-mnemonic route — drives `AuthContext.recover` to
 * rotate the password envelope on the server (via 1.16c3c3a's
 * `/password/reset/{start,finish}` pair) and re-establish a
 * session on this device with a freshly-derived device root key.
 *
 * Single-device limitation, same as `/login` (1.16c3b): the
 * account id must already be cached on this device. Cross-device
 * recovery arrives with 1.16d's discovery service.
 *
 * The recovery flow only needs one PoW solve (gating `/start`),
 * so it's a few hundred ms faster than signup. The 24-word
 * textarea is forgiving about whitespace — `fromPhrase`
 * normalises newlines, tabs, and runs of spaces.
 *
 * ## Phase 1.21c — social recovery (Shamir 3-of-5)
 *
 * The form now offers two input modes:
 *
 *   * **Phrase** — paste the original 24-word mnemonic.
 *     Unchanged from 1.16c3c3.
 *   * **Shares** — paste 3+ Shamir shares, each on its own
 *     line, of the form `<index>:<base64>`. The
 *     [`combineSharesToMnemonic`] helper reconstructs the
 *     entropy + rebuilds the canonical phrase, then drives
 *     the same `AuthContext.recover(phrase, …)` path. A
 *     wrong / insufficient share count produces a
 *     syntactically-valid phrase that fails to OPAQUE-
 *     authenticate; the auth-service rejection surfaces here
 *     as the standard "invalid recovery" error (which is the
 *     right collapse — distinguishing "wrong shares" from
 *     "wrong phrase" would leak the social-recovery layer to
 *     the server).
 *
 * ## ADR-0018 factor (a) — restore via another device
 *
 * A third input mode, "Another device", covers the NO-PASSWORD
 * entry of the recovery cascade: the user types only the username,
 * shows a pairing QR to a device that is still signed in, and that
 * device transfers the AccountIdentity seed + a single-use access
 * grant over the E2EE rendezvous channel. The grant redeems
 * anonymously for a session token (`/device/authorize/redeem`), so
 * neither a password nor the phrase is ever typed here.
 *
 * ## ADR-0018 factor (c) — authenticator (TOTP) code
 *
 * A fourth input mode, "Authenticator": username + the current
 * 6-digit code from a previously-enrolled authenticator app.
 * `recoverWithTotp` grants ACCESS ONLY (a session token — no
 * password, no identity material); the ADR-0017 identity
 * acquisition then either finishes the session from a cached seed
 * (same-device case) or flips to `needs-recovery-phrase`, where the
 * RestoreIdentityPrompt offers phrase / another-device. No manual
 * navigation here — the route's `RequireAnonymous` guard reacts to
 * the state flip (home on `session`; the restore prompt in place on
 * `needs-recovery-phrase`).
 */
import { UsernameHint } from "../profile/UsernameHint";
import { requireValidUsername } from "../profile/username-field";
import { createSignal, Match, Show, Switch, type JSX } from "solid-js";
import { A, useNavigate } from "@solidjs/router";
import { paths } from "./paths";
import { AuthLayout } from "../ui/AuthLayout";
import { PhraseInput } from "../ui/PhraseInput";
import { WordSlots } from "../ui/WordSlots";
import { useAuth } from "../auth/AuthContext";
import {
  WORD_VAULT_DESTRUCTION_CAP,
  wordVaultErrorMessage,
  wordsAreAnswerable,
  type WordVaultChallenge,
} from "../recovery/word-vault";
import { AuthHttpError } from "../auth/client";
import { normalizeTotpCode } from "../auth/totp-code";
import { isPlausibleContact } from "../auth/contact-shape";
import { ContactKind } from "../auth/wire";
import { base64Encode } from "../auth/account-id";
import { combineSharesToMnemonic } from "../recovery/shamir";
import { DiscoveryClient } from "../discovery/client";
import {
  prepareAnonymousJoinFlow,
  type AnonymousJoinSession,
  type JoinRecoveryOutcome,
} from "../devices/orchestrator";
import RestoreViaDevicePanel, {
  type DeviceRestoreFlowHandle,
} from "../devices/RestoreViaDevicePanel";

/** Which input mode the user is filling. */
type RecoverMode = "words" | "phrase" | "shares" | "device" | "totp" | "contact";

export default function Recover(): JSX.Element {
  const auth = useAuth();
  const navigate = useNavigate();

  const [mode, setMode] = createSignal<RecoverMode>("words");
  const [phrase, setPhrase] = createSignal("");
  const [sharesInput, setSharesInput] = createSignal("");
  const [password, setPassword] = createSignal("");
  const [confirm, setConfirm] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);

  const cachedUsername = (): string | null => {
    const s = auth.state();
    if (s.kind === "anon" && s.cachedUsername !== undefined) {
      return s.cachedUsername;
    }
    return null;
  };

  // ADR-0017: recover now resolves the account_id via
  // discovery.lookup on a fresh device, so we collect the username
  // here. Pre-filled from the device cache when present.
  const [username, setUsername] = createSignal(cachedUsername() ?? "");

  async function onSubmit(e: SubmitEvent): Promise<void> {
    e.preventDefault();
    setError(null);
    if (password() !== confirm()) {
      setError("Passwords do not match.");
      return;
    }
    if (password().length < 8) {
      setError("Password must be at least 8 characters.");
      return;
    }
    if (username().trim().length === 0) {
      setError("Enter the username of the account to recover.");
      return;
    }
    // ADR-0048 §4 — no lookup for a name that fails the username rule.
    let canonicalUsername: string;
    try {
      canonicalUsername = await requireValidUsername(username());
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
      return;
    }
    setBusy(true);
    try {
      // Resolve the phrase. Phrase mode passes it through
      // verbatim; shares mode reconstructs the entropy and
      // re-derives the phrase via BIP39's checksum logic.
      let resolved: string;
      if (mode() === "shares") {
        const lines = sharesInput()
          .split(/\r?\n/)
          .map((l) => l.trim())
          .filter((l) => l.length > 0);
        if (lines.length < 2) {
          throw new Error(
            "At least 2 shares required (default Lakko config is 3-of-5).",
          );
        }
        try {
          resolved = await combineSharesToMnemonic(lines);
        } catch (err) {
          throw new Error(
            `Couldn't combine shares: ${
              err instanceof Error ? err.message : String(err)
            }`,
          );
        }
      } else {
        resolved = phrase();
      }
      await auth.recover(resolved, password(), canonicalUsername);
      // Drop the phrase / shares + password fields immediately
      // — the mnemonic is a long-term secret the user has to
      // keep on paper; we don't need it any more after the
      // server has accepted the rotation.
      setPhrase("");
      setSharesInput("");
      setPassword("");
      setConfirm("");
      navigate(paths.home, { replace: true });
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <AuthLayout>
        <h1>Recover your account</h1>
        <p class="auth-lede">
          Choose how to prove this account is yours. <strong>Three words</strong>{" "}
          of your recovery phrase, the <strong>full phrase</strong> or{" "}
          <strong>another signed-in device</strong> restore your identity, so
          your contacts keep trusting you. An <strong>authenticator</strong> or{" "}
          <strong>email/SMS code</strong> restores access only; if this device
          doesn't already hold your identity, you'll then save a new recovery
          phrase.
        </p>
        <ModeTabs mode={mode} setMode={setMode} busy={busy} />
        <Switch
          fallback={
            <RecoverForm
              cachedUsername={cachedUsername()}
              username={username}
              setUsername={setUsername}
              mode={mode}
              setMode={setMode}
              phrase={phrase}
              setPhrase={setPhrase}
              sharesInput={sharesInput}
              setSharesInput={setSharesInput}
              password={password}
              setPassword={setPassword}
              confirm={confirm}
              setConfirm={setConfirm}
              busy={busy}
              error={error}
              onSubmit={onSubmit}
            />
          }
        >
          <Match when={mode() === "words"}>
            <WordsRecoverSection
              username={username}
              setUsername={setUsername}
              cachedUsername={cachedUsername()}
              onDone={() => navigate(paths.home, { replace: true })}
              onUnavailable={() => setMode("phrase")}
            />
          </Match>
          <Match when={mode() === "device"}>
            <DeviceRestoreSection
              username={username}
              setUsername={setUsername}
              onDone={() => navigate(paths.home, { replace: true })}
            />
          </Match>
          <Match when={mode() === "totp"}>
            <TotpRecoverSection
              username={username}
              setUsername={setUsername}
            />
          </Match>
          <Match when={mode() === "contact"}>
            <ContactRecoverSection
              username={username}
              setUsername={setUsername}
            />
          </Match>
        </Switch>
    </AuthLayout>
  );
}

// ---- ADR-0049: three words of the recovery phrase ------------------

/**
 * Forgot-password recovery with three words. Step 1 resolves the username
 * to an account and asks the server which words to type; step 2 takes the
 * three words plus a new password, opens the vault into the full phrase, and
 * runs the SAME `auth.recover` the 24-word tab runs (so the published
 * identity check and the password rotation are unchanged).
 */
function WordsRecoverSection(props: {
  readonly username: () => string;
  readonly setUsername: (v: string) => void;
  readonly cachedUsername: string | null;
  readonly onDone: () => void;
  readonly onUnavailable: () => void;
}): JSX.Element {
  const auth = useAuth();
  const [accountIdB64, setAccountIdB64] = createSignal<string | null>(null);
  const [canonicalUsername, setCanonicalUsername] = createSignal("");
  const [challenge, setChallenge] = createSignal<WordVaultChallenge | null>(null);
  const [words, setWords] = createSignal<string[]>(["", "", ""]);
  const [password, setPassword] = createSignal("");
  const [confirm, setConfirm] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  const [misses, setMisses] = createSignal(0);

  async function onStart(e: SubmitEvent): Promise<void> {
    e.preventDefault();
    setError(null);
    if (props.username().trim().length === 0) {
      setError("Enter the username of the account to recover.");
      return;
    }
    setBusy(true);
    try {
      const canonical = await requireValidUsername(props.username());
      const id = await auth.resolveRecoverAccountId(canonical);
      const c = await auth.wordVaultStart(id);
      setCanonicalUsername(canonical);
      setAccountIdB64(id);
      setChallenge(c);
    } catch (err) {
      if (err instanceof AuthHttpError && err.status === 503) {
        setError("Three-word recovery is not available on this server.");
        props.onUnavailable();
      } else {
        setError(wordVaultErrorMessage(err, null));
      }
    } finally {
      setBusy(false);
    }
  }

  async function onAnswer(e: SubmitEvent): Promise<void> {
    e.preventDefault();
    setError(null);
    const id = accountIdB64();
    const c = challenge();
    if (id === null || c === null) return;
    if (password() !== confirm()) {
      setError("Passwords do not match.");
      return;
    }
    if (password().length < 8) {
      setError("Password must be at least 8 characters.");
      return;
    }
    setBusy(true);
    try {
      const fullPhrase = await auth.wordVaultOpen(id, c, words());
      await auth.recover(fullPhrase, password(), canonicalUsername());
      setWords(["", "", ""]);
      setPassword("");
      setConfirm("");
      props.onDone();
    } catch (err) {
      if (err instanceof AuthHttpError && err.status === 401) {
        const next = misses() + 1;
        setMisses(next);
        setWords(["", "", ""]);
        if (next >= WORD_VAULT_DESTRUCTION_CAP) {
          setError(
            "The three-word shortcut is no longer available for this account. Use your full 24-word phrase or another device.",
          );
          props.onUnavailable();
        } else {
          setError(wordVaultErrorMessage(err, WORD_VAULT_DESTRUCTION_CAP - next));
        }
      } else {
        setError(wordVaultErrorMessage(err, null));
      }
    } finally {
      setBusy(false);
    }
  }

  return (
    <>
      <p style={{ color: "var(--surface-muted)", "margin-top": 0 }}>
        Type three words from your recovery phrase and choose a new password.
        <Show when={props.cachedUsername !== null}>
          {" "}
          Account on this device: <strong>@{props.cachedUsername}</strong>.
        </Show>
      </p>
      <Show
        when={challenge() !== null}
        fallback={
          <form onSubmit={onStart}>
            <label>
              Username
              <input
                type="text"
                autocomplete="username"
                required
                value={props.username()}
                onInput={(e) => props.setUsername(e.currentTarget.value)}
                disabled={busy()}
                placeholder="alice"
              />
            </label>
            <UsernameHint value={props.username} />
            <Show when={error() !== null}>
              <div class="form-error">{error()}</div>
            </Show>
            <button class="primary" type="submit" disabled={busy()}>
              {busy() ? "Looking up…" : "Continue"}
            </button>
          </form>
        }
      >
        <form onSubmit={onAnswer}>
          <p style={{ color: "var(--surface-muted)", "margin-top": 0 }}>
            <strong>@{canonicalUsername()}</strong>: type words{" "}
            <strong>{challenge()?.positions.join(", ")}</strong> of your recovery
            phrase.
          </p>
          <WordSlots
            positions={() => challenge()?.positions ?? []}
            value={words}
            setValue={setWords}
            disabled={busy}
          />
          <label>
            New password
            <input
              type="password"
              autocomplete="new-password"
              required
              value={password()}
              onInput={(e) => setPassword(e.currentTarget.value)}
              disabled={busy()}
            />
          </label>
          <label>
            Confirm new password
            <input
              type="password"
              autocomplete="new-password"
              required
              value={confirm()}
              onInput={(e) => setConfirm(e.currentTarget.value)}
              disabled={busy()}
            />
          </label>
          <Show when={error() !== null}>
            <div class="form-error">{error()}</div>
          </Show>
          <button
            class="primary"
            type="submit"
            disabled={busy() || !wordsAreAnswerable(words())}
          >
            {busy() ? "Checking…" : "Recover account"}
          </button>
          <Show when={misses() > 0}>
            <p class="form-info">{WORD_VAULT_DESTRUCTION_CAP - misses()} tries left.</p>
          </Show>
        </form>
      </Show>
      <p class="form-footer">
        Remembered your password? <A href={paths.login}>Log in</A>
      </p>
    </>
  );
}

// ---- ADR-0018 factor (a): restore via another device -------------

function DeviceRestoreSection(props: {
  readonly username: () => string;
  readonly setUsername: (v: string) => void;
  readonly onDone: () => void;
}): JSX.Element {
  const auth = useAuth();
  const discovery = new DiscoveryClient();

  // Captured across prepare → onRestored (the panel guarantees the
  // ordering; components run once in Solid, so plain variables work).
  let resolved: {
    accountIdBytes: Uint8Array;
    accountIdB64: string;
    username: string;
  } | null = null;
  let liveSession: AnonymousJoinSession | null = null;

  async function prepare(): Promise<DeviceRestoreFlowHandle> {
    const trimmed = props.username().trim();
    if (trimmed.length === 0) {
      throw new Error("Enter the username of the account to restore.");
    }
    // ADR-0048 §4 — throws the reason copy; no lookup is sent.
    const lookup = await discovery.lookup(await requireValidUsername(trimmed));
    if (lookup.account === null) {
      throw new Error("Unknown username — check the spelling.");
    }
    const accountIdBytes = lookup.account;
    const accountIdB64 = base64Encode(accountIdBytes);
    resolved = {
      accountIdBytes,
      accountIdB64,
      username: lookup.normalisedUsername,
    };
    // Anonymous join: the DS rendezvous mailbox is pseudonym-keyed
    // and auth-free; no device key exists yet, so the DeviceIdentity
    // seed stays in memory until `completeFromDeviceRestore`
    // bootstraps the key and `persistTo` lands the state.
    const session = await prepareAnonymousJoinFlow({
      accountIdBytes,
      accountIdB64,
    });
    liveSession = session;
    return {
      qrPayload: session.qrPayload,
      awaitWelcome: (signal) => session.awaitWelcome(signal),
      accept: async (welcome, welcomeCursor) => {
        const outcome = await session.accept(welcome, { welcomeCursor });
        return outcome.recovery;
      },
      dispose: () => {
        session.dispose();
        liveSession = null;
      },
    };
  }

  async function onRestored(outcome: JoinRecoveryOutcome): Promise<void> {
    if (resolved === null) {
      outcome.phraseSeed.fill(0);
      throw new Error("internal: no resolved account for restore");
    }
    // The redeemed session token is the ONLY access on this path —
    // there is no password. Without it the restore cannot complete.
    if (
      outcome.sessionToken === null ||
      outcome.sessionExpiresAtMs === null
    ) {
      outcome.phraseSeed.fill(0);
      throw new Error(
        "Your other device transferred the identity, but the access " +
          "grant could not be redeemed. Try again, or use your " +
          "recovery phrase.",
      );
    }
    const session = liveSession;
    await auth.completeFromDeviceRestore({
      accountIdBytes: resolved.accountIdBytes,
      accountIdB64: resolved.accountIdB64,
      username: resolved.username,
      phraseSeed: outcome.phraseSeed,
      sessionToken: outcome.sessionToken,
      sessionExpiresAtMs: outcome.sessionExpiresAtMs,
      ...(session !== null
        ? {
            persistJoinState: (deviceKey: CryptoKey): Promise<void> =>
              session.persistTo(deviceKey),
          }
        : {}),
    });
    props.onDone();
  }

  return (
    <div>
      <p style={{ color: "var(--surface-muted)", "margin-top": 0 }}>
        If another device is still signed in to your account, it can
        restore this one: enter your username, then confirm the request
        on that device and compare the code it shows. Identity and access
        transfer end-to-end encrypted; the server never sees them. No
        camera needed; a QR code is there if you prefer.
      </p>
      <label>
        Username
        <input
          type="text"
          autocomplete="username"
          required
          value={props.username()}
          onInput={(e) => props.setUsername(e.currentTarget.value)}
          placeholder="alice"
        />
      </label>
      <UsernameHint value={props.username} />
      <RestoreViaDevicePanel
        prepare={prepare}
        onRestored={onRestored}
        requestAccount={() => resolved?.accountIdBytes ?? null}
      />
    </div>
  );
}

// ---- ADR-0018 factor (c): authenticator (TOTP) code ---------------

/** Map a `recoverWithTotp` failure onto a friendly inline message.
 *  The server is enumeration-safe: 401 covers unknown-account /
 *  unconfirmed-enrollment / wrong-code / replayed-code uniformly. */
function totpRecoverErrorMessage(err: unknown): string {
  if (err instanceof AuthHttpError) {
    if (err.status === 401) {
      return "Wrong or expired code. Check your authenticator app and try again.";
    }
    if (err.status === 429) {
      return "Too many attempts — wait 15 minutes and try again.";
    }
    if (err.status === 503) {
      return "Authenticator recovery is not available on this server.";
    }
  }
  return err instanceof Error ? err.message : String(err);
}

function TotpRecoverSection(props: {
  readonly username: () => string;
  readonly setUsername: (v: string) => void;
}): JSX.Element {
  const auth = useAuth();
  const [code, setCode] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);

  async function onSubmit(e: SubmitEvent): Promise<void> {
    e.preventDefault();
    setError(null);
    let trimmedUser = props.username().trim();
    if (trimmedUser.length === 0) {
      setError("Enter the username of the account to recover.");
      return;
    }
    // ADR-0048 §4 — no lookup for a name that fails the username rule.
    try {
      trimmedUser = await requireValidUsername(trimmedUser);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
      return;
    }
    const normalized = normalizeTotpCode(code());
    if (normalized === null) {
      setError("Enter the 6-digit code from your authenticator app.");
      return;
    }
    setBusy(true);
    try {
      await auth.recoverWithTotp(trimmedUser, normalized);
      setCode("");
      // No manual navigation: the auth state just flipped to either
      // `session` (the route's RequireAnonymous guard bounces home)
      // or `needs-recovery-phrase` (the guard renders the
      // RestoreIdentityPrompt in place — phrase or another-device
      // completes the identity restore).
    } catch (err) {
      setError(totpRecoverErrorMessage(err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div>
      <p style={{ color: "var(--surface-muted)", "margin-top": 0 }}>
        Recover access with the 6-digit code from your authenticator
        app (enrolled earlier under Devices). This restores access to
        your account; your secure identity is then restored from your
        recovery phrase or another signed-in device.
      </p>
      <form onSubmit={(e) => void onSubmit(e)}>
        <label>
          Username
          <input
            type="text"
            autocomplete="username"
            required
            value={props.username()}
            onInput={(e) => props.setUsername(e.currentTarget.value)}
            disabled={busy()}
            placeholder="alice"
          />
        </label>
        <UsernameHint value={props.username} />
        <label>
          Authenticator code
          <input
            type="text"
            inputmode="numeric"
            autocomplete="one-time-code"
            pattern="[0-9]{6}"
            maxlength="6"
            required
            value={code()}
            onInput={(e) => setCode(e.currentTarget.value)}
            disabled={busy()}
            placeholder="123456"
            style={{
              "font-family":
                "ui-monospace, SFMono-Regular, Menlo, monospace",
              "letter-spacing": "0.25em",
            }}
          />
        </label>
        <Show when={error() !== null}>
          <div class="form-error">{error()}</div>
        </Show>
        <button class="primary" type="submit" disabled={busy()}>
          {busy() ? "Verifying…" : "Recover access"}
        </button>
      </form>
    </div>
  );
}

// ---- ADR-0018 factor (d): email / SMS one-time code --------------

/** Map a phone/email OTP recover failure onto a friendly inline
 *  message. The server is enumeration-safe: 401 covers unknown
 *  account / no-live-challenge / wrong-code / replayed-code
 *  uniformly. */
function contactRecoverErrorMessage(err: unknown): string {
  if (err instanceof AuthHttpError) {
    if (err.status === 401) {
      return "Wrong or expired code. Check the code we sent and try again.";
    }
    if (err.status === 429) {
      return "Too many attempts — wait, then request a fresh code.";
    }
    if (err.status === 503) {
      return "Email / SMS recovery is not available on this server.";
    }
    if (err.status === 400) {
      return "That contact looks malformed. Check it and try again.";
    }
  }
  return err instanceof Error ? err.message : String(err);
}

function ContactRecoverSection(props: {
  readonly username: () => string;
  readonly setUsername: (v: string) => void;
}): JSX.Element {
  const auth = useAuth();
  // Two steps: collect username + contact and send a code, then
  // collect the 6-digit code and finish.
  type Step = "request" | "code";
  const [step, setStep] = createSignal<Step>("request");
  const [kind, setKind] = createSignal<ContactKind>(ContactKind.Email);
  const [contact, setContact] = createSignal("");
  const [code, setCode] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);

  async function onSendCode(e: SubmitEvent): Promise<void> {
    e.preventDefault();
    setError(null);
    let trimmedUser = props.username().trim();
    if (trimmedUser.length === 0) {
      setError("Enter the username of the account to recover.");
      return;
    }
    // ADR-0048 §4 — no lookup for a name that fails the username rule.
    try {
      trimmedUser = await requireValidUsername(trimmedUser);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
      return;
    }
    // Light client-side shape check — saves a PoW solve on an
    // obviously-malformed contact. The server is authoritative.
    if (!isPlausibleContact(kind(), contact())) {
      setError(
        kind() === ContactKind.Email
          ? "Enter a valid email address."
          : "Enter a phone number in international format (e.g. +15551234567).",
      );
      return;
    }
    setBusy(true);
    try {
      await auth.recoverWithContactStart(trimmedUser, kind(), contact());
      // Enumeration-safe: we advance regardless of whether the
      // contact matched — the server reveals nothing either way.
      setCode("");
      setStep("code");
    } catch (err) {
      setError(contactRecoverErrorMessage(err));
    } finally {
      setBusy(false);
    }
  }

  async function onSubmitCode(e: SubmitEvent): Promise<void> {
    e.preventDefault();
    setError(null);
    const normalized = normalizeTotpCode(code());
    if (normalized === null) {
      setError("Enter the 6-digit code we sent.");
      return;
    }
    setBusy(true);
    try {
      await auth.recoverWithContactFinish(normalized);
      setCode("");
      // No manual navigation: the auth state just flipped to either
      // `session` (the route's RequireAnonymous guard bounces home)
      // or `needs-recovery-phrase` (the guard renders the
      // RestoreIdentityPrompt in place).
    } catch (err) {
      setError(contactRecoverErrorMessage(err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div>
      <p style={{ color: "var(--surface-muted)", "margin-top": 0 }}>
        Recover access with a one-time code sent to a phone or email you
        enrolled earlier (under Devices). This restores access to your
        account; your secure identity is then restored from your recovery
        phrase or another signed-in device.
      </p>
      <Switch>
        <Match when={step() === "request"}>
          <form onSubmit={(e) => void onSendCode(e)}>
            <label>
              Username
              <input
                type="text"
                autocomplete="username"
                required
                value={props.username()}
                onInput={(e) => props.setUsername(e.currentTarget.value)}
                disabled={busy()}
                placeholder="alice"
              />
            </label>
            <UsernameHint value={props.username} />
            <KindToggle kind={kind} setKind={setKind} busy={busy} />
            <label>
              {kind() === ContactKind.Email ? "Email" : "Phone number"}
              <input
                type={kind() === ContactKind.Email ? "email" : "tel"}
                autocomplete={kind() === ContactKind.Email ? "email" : "tel"}
                required
                value={contact()}
                onInput={(e) => setContact(e.currentTarget.value)}
                disabled={busy()}
                placeholder={
                  kind() === ContactKind.Email
                    ? "alice@example.com"
                    : "+15551234567"
                }
              />
            </label>
            <Show when={error() !== null}>
              <div class="form-error">{error()}</div>
            </Show>
            <button class="primary" type="submit" disabled={busy()}>
              {busy() ? "Sending…" : "Send code"}
            </button>
          </form>
        </Match>
        <Match when={step() === "code"}>
          <form onSubmit={(e) => void onSubmitCode(e)}>
            <p>
              If that {kind() === ContactKind.Email ? "email" : "number"} is
              enrolled for this account, a 6-digit code is on its way. Enter
              it below.
            </p>
            <label>
              Recovery code
              <input
                type="text"
                inputmode="numeric"
                autocomplete="one-time-code"
                pattern="[0-9]{6}"
                maxlength="6"
                required
                value={code()}
                onInput={(e) => setCode(e.currentTarget.value)}
                disabled={busy()}
                placeholder="123456"
                style={{
                  "font-family":
                    "ui-monospace, SFMono-Regular, Menlo, monospace",
                  "letter-spacing": "0.25em",
                }}
              />
            </label>
            <Show when={error() !== null}>
              <div class="form-error">{error()}</div>
            </Show>
            <button class="primary" type="submit" disabled={busy()}>
              {busy() ? "Verifying…" : "Recover access"}
            </button>{" "}
            <button
              type="button"
              disabled={busy()}
              onClick={() => {
                setError(null);
                setCode("");
                setStep("request");
              }}
            >
              Use a different contact
            </button>
          </form>
        </Match>
      </Switch>
    </div>
  );
}

function KindToggle(props: {
  kind: () => ContactKind;
  setKind: (v: ContactKind) => void;
  busy: () => boolean;
}): JSX.Element {
  return (
    <div
      role="tablist"
      style={{ display: "flex", gap: "0.4rem", margin: "0.5rem 0" }}
    >
      <TabButton
        active={props.kind() === ContactKind.Email}
        disabled={props.busy()}
        onClick={() => props.setKind(ContactKind.Email)}
        label="Email"
      />
      <TabButton
        active={props.kind() === ContactKind.Phone}
        disabled={props.busy()}
        onClick={() => props.setKind(ContactKind.Phone)}
        label="SMS"
      />
    </div>
  );
}

interface RecoverFormProps {
  readonly cachedUsername: string | null;
  readonly username: () => string;
  readonly setUsername: (v: string) => void;
  readonly mode: () => RecoverMode;
  readonly setMode: (v: RecoverMode) => void;
  readonly phrase: () => string;
  readonly setPhrase: (v: string) => void;
  readonly sharesInput: () => string;
  readonly setSharesInput: (v: string) => void;
  readonly password: () => string;
  readonly setPassword: (v: string) => void;
  readonly confirm: () => string;
  readonly setConfirm: (v: string) => void;
  readonly busy: () => boolean;
  readonly error: () => string | null;
  readonly onSubmit: (e: SubmitEvent) => void;
}

function RecoverForm(props: RecoverFormProps): JSX.Element {
  return (
    <>
      <p style={{ color: "var(--surface-muted)", "margin-top": 0 }}>
        Recover your account using the full 24-word phrase or 3+
        social-recovery shares. We'll prove ownership to the server
        and rotate the password envelope.{" "}
        <Show when={props.cachedUsername !== null}>
          Account on this device:{" "}
          <strong>@{props.cachedUsername}</strong>.
        </Show>
      </p>
      <form onSubmit={props.onSubmit}>
        <label>
          Username
          <input
            type="text"
            autocomplete="username"
            required
            value={props.username()}
            onInput={(e) => props.setUsername(e.currentTarget.value)}
            disabled={props.busy()}
            placeholder="alice"
          />
        </label>
        <UsernameHint value={props.username} />
        <Show
          when={props.mode() === "phrase"}
          fallback={
            <SharesField
              value={props.sharesInput}
              setValue={props.setSharesInput}
              busy={props.busy}
            />
          }
        >
          <PhraseField
            value={props.phrase}
            setValue={props.setPhrase}
            busy={props.busy}
          />
        </Show>
        <label>
          New password
          <input
            type="password"
            autocomplete="new-password"
            required
            value={props.password()}
            onInput={(e) => props.setPassword(e.currentTarget.value)}
            disabled={props.busy()}
          />
        </label>
        <label>
          Confirm new password
          <input
            type="password"
            autocomplete="new-password"
            required
            value={props.confirm()}
            onInput={(e) => props.setConfirm(e.currentTarget.value)}
            disabled={props.busy()}
          />
        </label>
        <Show when={props.error() !== null}>
          <div class="form-error">{props.error()}</div>
        </Show>
        <button class="primary" type="submit" disabled={props.busy()}>
          {props.busy() ? "Verifying…" : "Recover account"}
        </button>
      </form>
      <p class="form-footer">
        Remembered your password? <A href={paths.login}>Log in</A>
      </p>
    </>
  );
}

function ModeTabs(props: {
  mode: () => RecoverMode;
  setMode: (v: RecoverMode) => void;
  busy: () => boolean;
}): JSX.Element {
  return (
    <div
      role="tablist"
      style={{
        display: "flex",
        "flex-wrap": "wrap",
        gap: "0.4rem",
        "margin-bottom": "0.75rem",
      }}
    >
      <TabButton
        active={props.mode() === "words"}
        disabled={props.busy()}
        onClick={() => props.setMode("words")}
        label="Three words"
      />
      <TabButton
        active={props.mode() === "phrase"}
        disabled={props.busy()}
        onClick={() => props.setMode("phrase")}
        label="All 24 words"
      />
      <TabButton
        active={props.mode() === "shares"}
        disabled={props.busy()}
        onClick={() => props.setMode("shares")}
        label="Recovery shares"
      />
      <TabButton
        active={props.mode() === "device"}
        disabled={props.busy()}
        onClick={() => props.setMode("device")}
        label="Another device"
      />
      <TabButton
        active={props.mode() === "totp"}
        disabled={props.busy()}
        onClick={() => props.setMode("totp")}
        label="Authenticator"
      />
      <TabButton
        active={props.mode() === "contact"}
        disabled={props.busy()}
        onClick={() => props.setMode("contact")}
        label="Email or SMS code"
      />
    </div>
  );
}

function TabButton(props: {
  active: boolean;
  disabled: boolean;
  onClick: () => void;
  label: string;
}): JSX.Element {
  return (
    <button
      type="button"
      role="tab"
      aria-selected={props.active}
      disabled={props.disabled}
      onClick={props.onClick}
      style={{
        padding: "0.4rem 0.75rem",
        "border-radius": "var(--radius-md)",
        border: "1px solid var(--surface-border)",
        background: props.active
          ? "var(--accent)"
          : "var(--surface-elevated)",
        color: props.active ? "var(--accent-fg)" : "inherit",
        cursor: props.disabled ? "wait" : "pointer",
        "font-size": "0.85rem",
      }}
    >
      {props.label}
    </button>
  );
}

function PhraseField(props: {
  value: () => string;
  setValue: (v: string) => void;
  busy: () => boolean;
}): JSX.Element {
  return <PhraseInput value={props.value} setValue={props.setValue} disabled={props.busy} />;
}

function SharesField(props: {
  value: () => string;
  setValue: (v: string) => void;
  busy: () => boolean;
}): JSX.Element {
  return (
    <label>
      Recovery shares (one per line)
      <textarea
        rows={6}
        placeholder={
          "3:abcdEFGH...\n" +
          "1:zzzzABCD...\n" +
          "5:mmmm0123..."
        }
        required
        value={props.value()}
        onInput={(e) => props.setValue(e.currentTarget.value)}
        disabled={props.busy()}
        autocomplete="off"
        spellcheck={false}
        style={{
          "font-family":
            "ui-monospace, SFMono-Regular, Menlo, monospace",
        }}
      />
      <small style={{ color: "var(--surface-muted)" }}>
        Paste at least the threshold number of shares (default
        Lakko config is 3-of-5). Each share is one line of the
        form <code>&lt;index&gt;:&lt;base64&gt;</code>.
      </small>
    </label>
  );
}
