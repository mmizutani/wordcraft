CREATE TABLE cloud_documents (
    owner TEXT NOT NULL,
    id TEXT NOT NULL,
    first_operation_id TEXT NOT NULL,
    name TEXT,
    revision INTEGER,
    object_key TEXT,
    byte_size INTEGER,
    saved_at INTEGER,
    deleted INTEGER NOT NULL DEFAULT 0 CHECK (deleted IN (0, 1)),
    PRIMARY KEY (owner, id),
    UNIQUE (owner, first_operation_id),
    CHECK (deleted = 1 OR (
        name IS NOT NULL AND revision > 0 AND revision IS NOT NULL AND object_key IS NOT NULL
        AND byte_size > 0 AND byte_size IS NOT NULL AND saved_at IS NOT NULL
    ))
);

CREATE INDEX cloud_documents_library ON cloud_documents (owner, saved_at DESC, id DESC) WHERE deleted = 0;

-- A pending create owns an object key before its document exists.
CREATE TABLE cloud_saves (
    owner TEXT NOT NULL,
    operation_id TEXT NOT NULL,
    document_id TEXT NOT NULL,
    base_revision INTEGER NOT NULL CHECK (base_revision >= 0),
    name TEXT NOT NULL,
    byte_size INTEGER NOT NULL CHECK (byte_size BETWEEN 1 AND 16777216),
    sha256 TEXT NOT NULL,
    object_key TEXT NOT NULL UNIQUE,
    lease_until INTEGER NOT NULL,
    outcome TEXT NOT NULL DEFAULT 'pending' CHECK (
        outcome IN ('pending', 'committed', 'stale', 'abandoned', 'invalid', 'too_large', 'deleted')
    ),
    settled INTEGER NOT NULL DEFAULT 0 CHECK (settled IN (0, 1)),
    cleanup TEXT NOT NULL DEFAULT 'none' CHECK (cleanup IN ('none', 'pending', 'done')),
    saved_at INTEGER,
    PRIMARY KEY (owner, operation_id)
);

CREATE INDEX cloud_saves_document ON cloud_saves (owner, document_id, base_revision);
CREATE INDEX cloud_saves_expired ON cloud_saves (owner, lease_until, operation_id) WHERE outcome = 'pending';
CREATE INDEX cloud_saves_cleanup ON cloud_saves (owner, document_id, operation_id) WHERE cleanup = 'pending';
