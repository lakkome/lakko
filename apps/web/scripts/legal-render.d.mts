// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/** Types for `legal-render.mjs`, so the vitest suite under `src/` type-checks. */
export declare const STUB_LINE: string;
export declare function escapeHtml(text: string): string;
export declare function stripHtmlComments(source: string): string;
export declare function isUnfinished(source: string): boolean;
export declare function renderInline(text: string): string;
export declare function renderMarkdown(source: string): { title: string | null; html: string };
export declare function htmlPage(page: { title: string; bodyHtml: string }): string;
export declare function legalPage(
  source: string,
  fallbackTitle: string,
): { html: string; stub: boolean };
