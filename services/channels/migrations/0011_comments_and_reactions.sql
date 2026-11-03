-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- ADR-0087 section 13a: comments and reactions under a channel post.
--
-- A subscriber has no MLS leaf, so its reactions and comments are rows here.
-- Every row names a write key derived from the writer's account seed for that
-- one write (`lakko_channels::write_key`): a reaction's key from the post and
-- the label, a comment's from the post and a random nonce. No key is reused
-- across rows, so nothing here links two rows to one person.
--
-- - `channel_reactions`: one row per `(post, writer_key, label)`. A label is
--   `BLAKE3-keyed(reaction_key, post || emoji)[..16]`; the service holds no
--   emoji and cannot compare labels between posts. Readers get a count per
--   label.
-- - `channel_comments`: an opaque sealed comment (at most 2,048 bytes,
--   `lakko_channels::comment`) under a post; `id` is the comment cursor,
--   global and ascending, so a page is "after this id".
-- - `channel_write_nonces`: the nonce of every accepted write proof, so a
--   captured request cannot be replayed inside its 120 s window (kept for
--   twice the window).
--
-- Both row tables cascade from `channel_posts`, so a post pruned after its 30
-- days takes its reactions and comments with it.

CREATE TABLE IF NOT EXISTS channel_reactions (
    channel_id     BINARY(16)      NOT NULL,
    post_cursor    BIGINT UNSIGNED NOT NULL,
    writer_key     BINARY(32)      NOT NULL
                     COMMENT 'Ed25519 key of this one reaction; links no person',
    label          BINARY(16)      NOT NULL
                     COMMENT 'keyed hash of post and emoji; the emoji is not here',
    created_at_ms  BIGINT UNSIGNED NOT NULL,
    PRIMARY KEY (channel_id, post_cursor, writer_key, label),
    KEY idx_channel_reactions_count (channel_id, post_cursor, label),
    CONSTRAINT fk_channel_reactions_post
        FOREIGN KEY (channel_id, post_cursor) REFERENCES channel_posts (channel_id, `cursor`)
        ON DELETE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

CREATE TABLE IF NOT EXISTS channel_comments (
    id             BIGINT UNSIGNED NOT NULL AUTO_INCREMENT
                     COMMENT 'the comment cursor',
    channel_id     BINARY(16)      NOT NULL,
    post_cursor    BIGINT UNSIGNED NOT NULL,
    writer_key     BINARY(32)      NOT NULL
                     COMMENT 'Ed25519 key of this one comment; signs its deletion',
    sealed         VARBINARY(2048) NOT NULL
                     COMMENT 'lakko_channels::comment sealed bytes; never opened here',
    created_at_ms  BIGINT UNSIGNED NOT NULL,
    PRIMARY KEY (id),
    KEY idx_channel_comments_page (channel_id, post_cursor, id),
    CONSTRAINT fk_channel_comments_post
        FOREIGN KEY (channel_id, post_cursor) REFERENCES channel_posts (channel_id, `cursor`)
        ON DELETE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

CREATE TABLE IF NOT EXISTS channel_write_nonces (
    nonce       BINARY(16)      NOT NULL,
    seen_at_ms  BIGINT UNSIGNED NOT NULL,
    PRIMARY KEY (nonce),
    KEY idx_channel_write_nonces_seen (seen_at_ms)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
