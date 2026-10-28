// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The context object alone, apart from its provider, so a bubble can ask for
 * the controller without importing the session machinery the provider needs
 * (`TranslationContext.tsx`). Outside the provider - every component test -
 * the answer is `undefined` and nothing is offered.
 */

import { createContext, useContext } from "solid-js";

import type { TranslationController } from "./controller";

export const TranslationContext = createContext<TranslationController>();

export function useTranslation(): TranslationController | undefined {
  return useContext(TranslationContext);
}
