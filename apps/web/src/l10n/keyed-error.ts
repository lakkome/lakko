// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * An error whose user-facing text is a catalog key (ADR-0058 section 6:
 * errors render from their code, never from `.message`).
 *
 * `.message` carries the text in the language current when the error was
 * thrown, for logs and for callers that still read it; a screen renders
 * {@link KeyedError.text} (or {@link keyedErrorText}), which re-renders the
 * key in the language current at display time.
 */
import { t, type L10nArgs } from "./index";

export class KeyedError extends Error {
  readonly key: string;
  readonly args: L10nArgs | undefined;

  constructor(key: string, args?: L10nArgs) {
    super(t(key, args));
    this.name = "KeyedError";
    this.key = key;
    this.args = args;
  }

  /** The message in the current language. */
  text(): string {
    return t(this.key, this.args);
  }
}

/** The text of a {@link KeyedError}, or `null` for any other value. */
export function keyedErrorText(err: unknown): string | null {
  return err instanceof KeyedError ? err.text() : null;
}
