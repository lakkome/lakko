// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0090 section 1 — the one-line preview of a content message (chat list,
 * notifications): `Poll: {question}`, `Checklist: {title}`, `Location`,
 * `Contact: {name}`, `Article: {title}`. Never JSON, never raw text.
 */
import { t } from "../l10n";
import type { MessageContent } from "../../wasm/lakko_ffi_wasm";

const KINDS = new Set(["poll", "checklist", "location", "contactCard", "article"]);

/** A stored content object back, or `undefined` when it is not shaped like
 *  one (only the discriminant and the field each kind is read through). */
export function contentFromPersisted(raw: unknown): MessageContent | undefined {
  if (raw === null || typeof raw !== "object") return undefined;
  const kind = (raw as { kind?: unknown }).kind;
  if (typeof kind !== "string" || !KINDS.has(kind)) return undefined;
  const r = raw as Record<string, unknown>;
  const field = kind === "contactCard" ? "card" : kind;
  if (r[field] === null || typeof r[field] !== "object") return undefined;
  return raw as MessageContent;
}

/** The preview line, or `null` when `raw` is not a content object. */
export function contentPreviewOf(raw: unknown): string | null {
  const c = contentFromPersisted(raw);
  if (c === undefined) return null;
  return contentPreview(c);
}

export function contentPreview(c: MessageContent): string {
  switch (c.kind) {
    case "poll":
      return t("preview.poll", { question: oneLine(c.poll.body.question) });
    case "checklist":
      return t("preview.checklist", { title: oneLine(c.checklist.body.title) });
    case "location":
      return t("preview.location");
    case "contactCard":
      return t("preview.contact", { name: oneLine(c.card.name) });
    case "article":
      return t("preview.article", { title: oneLine(c.article.title) });
  }
}

function oneLine(s: string): string {
  return s.replace(/\s+/g, " ").trim();
}
