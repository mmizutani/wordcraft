-- Compact operation identity survives pruning the corresponding journal payload.
CREATE TABLE cloud_room_identities (
    owner TEXT NOT NULL,
    document_id TEXT NOT NULL,
    operation_id TEXT NOT NULL,
    revision INTEGER NOT NULL CHECK (revision > 0),
    actor TEXT NOT NULL,
    fingerprint TEXT NOT NULL,
    PRIMARY KEY (owner, document_id, operation_id),
    FOREIGN KEY (owner, document_id) REFERENCES cloud_rooms (owner, document_id) ON DELETE CASCADE
);
