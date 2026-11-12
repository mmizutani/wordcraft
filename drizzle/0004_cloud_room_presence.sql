CREATE TABLE cloud_room_presence (
    owner TEXT NOT NULL,
    document_id TEXT NOT NULL,
    actor TEXT NOT NULL,
    session_id TEXT NOT NULL,
    grant_version INTEGER NOT NULL,
    revision INTEGER NOT NULL,
    position_json TEXT NOT NULL,
    display_name TEXT,
    updated_ms INTEGER NOT NULL,
    expires_ms INTEGER NOT NULL,
    PRIMARY KEY (owner, document_id, session_id),
    FOREIGN KEY (owner, document_id) REFERENCES cloud_rooms(owner, document_id) ON DELETE CASCADE
);
