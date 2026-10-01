// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * A channel topic's name in the interface language (ADR-0059, ADR-0061).
 *
 * The core's `channelTopicNames()` / `channelTopicName(id)` answer English;
 * they are used for the list of ids only. A topic is drawn from its id through
 * this table, and an id this client does not know reads as "Other".
 */
import { t } from "../l10n";

const TOPIC_KEYS: Readonly<Record<number, string>> = {
  0: "discover.topic.other",
  1: "discover.topic.news",
  2: "discover.topic.technology",
  3: "discover.topic.science",
  4: "discover.topic.business",
  5: "discover.topic.finance",
  6: "discover.topic.politics",
  7: "discover.topic.sports",
  8: "discover.topic.gaming",
  9: "discover.topic.music",
  10: "discover.topic.film_tv",
  11: "discover.topic.books",
  12: "discover.topic.art_design",
  13: "discover.topic.photography",
  14: "discover.topic.food",
  15: "discover.topic.travel",
  16: "discover.topic.health",
  17: "discover.topic.education",
  18: "discover.topic.humour",
  19: "discover.topic.lifestyle",
  20: "discover.topic.nature",
  21: "discover.topic.religion",
  22: "discover.topic.local",
  23: "discover.topic.software_development",
};

/** The name of a topic by id; `null` and unknown ids read as "Other". */
export function topicLabel(id: number | null): string {
  return t((id === null ? undefined : TOPIC_KEYS[id]) ?? "discover.topic.other");
}
