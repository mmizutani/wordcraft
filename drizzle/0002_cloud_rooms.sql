CREATE TABLE cloud_rooms (
    owner TEXT NOT NULL,
    document_id TEXT NOT NULL,
    file_revision INTEGER NOT NULL CHECK (file_revision > 0),
    checkpoint_revision INTEGER NOT NULL DEFAULT 0 CHECK (checkpoint_revision >= 0),
    checkpoint_key TEXT NOT NULL UNIQUE,
    checkpoint_size INTEGER NOT NULL CHECK (checkpoint_size BETWEEN 1 AND 16777216),
    checkpoint_sha256 TEXT NOT NULL,
    head INTEGER NOT NULL DEFAULT 0 CHECK (head >= checkpoint_revision),
    tail_bytes INTEGER NOT NULL DEFAULT 0 CHECK (tail_bytes >= 0),
    PRIMARY KEY (owner, document_id),
    FOREIGN KEY (owner, document_id) REFERENCES cloud_documents (owner, id)
);

CREATE TABLE cloud_room_changes (
    owner TEXT NOT NULL,
    document_id TEXT NOT NULL,
    revision INTEGER NOT NULL CHECK (revision > 0),
    actor TEXT NOT NULL,
    operation_id TEXT NOT NULL,
    fingerprint TEXT NOT NULL,
    change_json TEXT NOT NULL,
    change_bytes INTEGER NOT NULL CHECK (change_bytes BETWEEN 1 AND 262144),
    PRIMARY KEY (owner, document_id, revision),
    UNIQUE (owner, document_id, operation_id),
    FOREIGN KEY (owner, document_id) REFERENCES cloud_rooms (owner, document_id) ON DELETE CASCADE
);

-- Reserve the key before R2 PUT so an interrupted activation can be fenced and cleaned.
CREATE TABLE cloud_room_candidates (
    owner TEXT NOT NULL,
    document_id TEXT NOT NULL,
    file_revision INTEGER NOT NULL,
    object_key TEXT NOT NULL PRIMARY KEY,
    lease_until INTEGER NOT NULL,
    settled INTEGER NOT NULL DEFAULT 0 CHECK (settled IN (0, 1)),
    fenced INTEGER NOT NULL DEFAULT 0 CHECK (fenced IN (0, 1)),
    FOREIGN KEY (owner, document_id) REFERENCES cloud_documents (owner, id)
);

CREATE INDEX cloud_room_candidates_cleanup ON cloud_room_candidates (owner, document_id, lease_until) WHERE fenced = 0;
