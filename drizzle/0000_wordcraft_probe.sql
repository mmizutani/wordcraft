CREATE TABLE IF NOT EXISTS wordcraft_probe (
    owner TEXT NOT NULL,
    id TEXT NOT NULL,
    object_key TEXT NOT NULL UNIQUE,
    PRIMARY KEY (owner, id)
);
