ALTER TABLE cloud_saves ADD COLUMN room_revision INTEGER CHECK (room_revision >= 0);
ALTER TABLE cloud_rooms ADD COLUMN published_room_revision INTEGER NOT NULL DEFAULT 0 CHECK (published_room_revision BETWEEN 0 AND head);
