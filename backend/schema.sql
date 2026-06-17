CREATE TABLE IF NOT EXISTS rooms (
    id            TEXT PRIMARY KEY,
    name          TEXT NOT NULL,
    slug          TEXT UNIQUE NOT NULL,
    password_hash TEXT,
    presenter_key TEXT,
    delivery_mode TEXT NOT NULL DEFAULT 'webrtc',
    waiting_room  INTEGER NOT NULL DEFAULT 0,
    noise_reduction   INTEGER NOT NULL DEFAULT 1,
    echo_cancellation INTEGER NOT NULL DEFAULT 1,
    push_to_talk      INTEGER NOT NULL DEFAULT 0,
    expires_at    DATETIME,
    status        TEXT NOT NULL DEFAULT 'pending',
    stream_key_id TEXT REFERENCES stream_keys(id) ON DELETE SET NULL,
    created_at    DATETIME DEFAULT CURRENT_TIMESTAMP,
    started_at    DATETIME,
    ended_at      DATETIME
);

CREATE TABLE IF NOT EXISTS stream_keys (
    id          TEXT PRIMARY KEY,
    name        TEXT NOT NULL,
    key_token   TEXT UNIQUE NOT NULL,
    room_id     TEXT REFERENCES rooms(id) ON DELETE SET NULL,
    created_at  DATETIME DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS participants (
    id          TEXT PRIMARY KEY,
    room_id     TEXT NOT NULL REFERENCES rooms(id) ON DELETE CASCADE,
    name        TEXT NOT NULL,
    role        TEXT NOT NULL DEFAULT 'viewer',
    is_admitted INTEGER NOT NULL DEFAULT 0,
    is_kicked   INTEGER NOT NULL DEFAULT 0,
    token       TEXT UNIQUE,
    joined_at   DATETIME DEFAULT CURRENT_TIMESTAMP
);

-- session_files holds both participant uploads and admin-library files.
-- A NULL room_id means the row is a library-only file (not tied to any room).
-- Admin-assigned rooms are recorded in room_files (many-to-many).
-- room_id uses SET NULL (not CASCADE) so deleting the originating room
-- demotes the file to library status when it's still attached to other
-- rooms via room_files. Unattached files are cleaned up explicitly by
-- cleanup_room_files before the room is deleted.
CREATE TABLE IF NOT EXISTS session_files (
    id            TEXT PRIMARY KEY,
    room_id       TEXT REFERENCES rooms(id) ON DELETE SET NULL,
    uploader_id   TEXT REFERENCES participants(id) ON DELETE SET NULL,
    original_name TEXT NOT NULL,
    stored_path   TEXT NOT NULL,
    mime_type     TEXT NOT NULL,
    size_bytes    INTEGER NOT NULL,
    content_hash  TEXT,
    -- is_shared = 0 means the file is a draft attached to a participant's
    -- chat input but not yet posted to the room. Drafts are hidden from
    -- chat history, the Files side panel, and the admin library. Flips to
    -- 1 once the participant sends the chat message.
    is_shared     INTEGER NOT NULL DEFAULT 1,
    created_at    DATETIME DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS room_files (
    room_id     TEXT NOT NULL REFERENCES rooms(id) ON DELETE CASCADE,
    file_id     TEXT NOT NULL REFERENCES session_files(id) ON DELETE CASCADE,
    assigned_at DATETIME DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (room_id, file_id)
);

CREATE TABLE IF NOT EXISTS settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

-- Single-admin WebAuthn passkeys. `credential` is the serde_json of a
-- webauthn-rs Passkey (includes the public key + signature counter, which is
-- updated in place on every successful authentication).
CREATE TABLE IF NOT EXISTS admin_passkeys (
    id            TEXT PRIMARY KEY,
    label         TEXT NOT NULL,
    credential    TEXT NOT NULL,
    created_at    DATETIME DEFAULT CURRENT_TIMESTAMP,
    last_used_at  DATETIME
);

CREATE TABLE IF NOT EXISTS chat_messages (
    id         TEXT PRIMARY KEY,
    room_id    TEXT NOT NULL REFERENCES rooms(id) ON DELETE CASCADE,
    name       TEXT NOT NULL,
    role       TEXT NOT NULL,
    text       TEXT NOT NULL,
    created_at DATETIME DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX IF NOT EXISTS idx_rooms_slug          ON rooms(slug);
CREATE INDEX IF NOT EXISTS idx_rooms_status        ON rooms(status);
CREATE INDEX IF NOT EXISTS idx_rooms_stream_key    ON rooms(stream_key_id);
CREATE INDEX IF NOT EXISTS idx_stream_keys_token   ON stream_keys(key_token);
CREATE INDEX IF NOT EXISTS idx_stream_keys_room    ON stream_keys(room_id);
CREATE INDEX IF NOT EXISTS idx_participants_room   ON participants(room_id);
CREATE INDEX IF NOT EXISTS idx_participants_token  ON participants(token);
CREATE INDEX IF NOT EXISTS idx_chat_messages_room  ON chat_messages(room_id, created_at);
-- session_files / room_files indexes are created in db.rs AFTER the
-- migrate_session_files_library step so they can reference content_hash.
