-- A share link identifies one live room, including when the requester is not its owner.
CREATE UNIQUE INDEX cloud_rooms_share_link ON cloud_rooms (document_id);

CREATE TABLE cloud_room_grants (
    owner TEXT NOT NULL,
    document_id TEXT NOT NULL,
    actor TEXT NOT NULL,
    id TEXT NOT NULL,
    display_name TEXT,
    role TEXT NOT NULL DEFAULT 'pending' CHECK (role IN ('pending', 'editor', 'viewer', 'revoked')),
    grant_version INTEGER NOT NULL DEFAULT 0 CHECK (grant_version BETWEEN 0 AND 9007199254740991),
    PRIMARY KEY (owner, document_id, actor),
    UNIQUE (owner, document_id, id),
    FOREIGN KEY (owner, document_id) REFERENCES cloud_rooms(owner, document_id) ON DELETE CASCADE,
    CHECK (actor <> owner)
);
