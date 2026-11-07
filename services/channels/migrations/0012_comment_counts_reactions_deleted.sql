-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- ADR-0087 phase B gaps (2026-11-05).
--
-- - `channel_comment_counts`: a maintained count of the comments under each
--   post, so a reader learns "Show {n} more comments" with the reaction
--   counts in one batched read, without a COUNT(*) over a busy post's rows.
--   Kept in the transaction that adds or deletes a comment; cascades from
--   `channel_posts` like the comments themselves.
-- - `channel_comment_reactions`: reactions on a comment (UX 13.1), one row
--   per `(comment, writer_key, label)`. A label here is keyed by a separate
--   comment-reaction key (`lakko_channels::CommentReactionKey`), so it is
--   never comparable with a post's labels or another comment's; the writer
--   key is a key for that one write. Cascades from `channel_comments`, so a
--   deleted comment, or a post pruned after its 30 days, takes its reactions
--   with it.
-- - `channel_deleted`: a tombstone per channel its owner deleted. The
--   `channels` row goes (and everything cascades from it); the tombstone
--   makes reads of the id answer `410 channel-deleted` and refuses the id to
--   a new registration. No foreign key: it outlives the row it records.

CREATE TABLE IF NOT EXISTS channel_comment_counts (
    channel_id     BINARY(16)      NOT NULL,
    post_cursor    BIGINT UNSIGNED NOT NULL,
    comments       BIGINT UNSIGNED NOT NULL DEFAULT 0,
    PRIMARY KEY (channel_id, post_cursor),
    CONSTRAINT fk_channel_comment_counts_post
        FOREIGN KEY (channel_id, post_cursor) REFERENCES channel_posts (channel_id, `cursor`)
        ON DELETE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

CREATE TABLE IF NOT EXISTS channel_comment_reactions (
    comment_id     BIGINT UNSIGNED NOT NULL
                     COMMENT 'channel_comments.id',
    writer_key     BINARY(32)      NOT NULL
                     COMMENT 'Ed25519 key of this one reaction; links no person',
    label          BINARY(16)      NOT NULL
                     COMMENT 'keyed hash of post, comment and emoji; the emoji is not here',
    created_at_ms  BIGINT UNSIGNED NOT NULL,
    PRIMARY KEY (comment_id, writer_key, label),
    KEY idx_channel_comment_reactions_count (comment_id, label),
    CONSTRAINT fk_channel_comment_reactions_comment
        FOREIGN KEY (comment_id) REFERENCES channel_comments (id)
        ON DELETE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

CREATE TABLE IF NOT EXISTS channel_deleted (
    channel_id     BINARY(16)      NOT NULL,
    deleted_at_ms  BIGINT UNSIGNED NOT NULL,
    PRIMARY KEY (channel_id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
