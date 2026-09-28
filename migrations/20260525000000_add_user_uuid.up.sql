ALTER TABLE users ADD COLUMN uuid VARCHAR(36) NOT NULL DEFAULT '' COMMENT 'UUID v7' AFTER id;
UPDATE users SET uuid = uuid_v7();
CREATE UNIQUE INDEX uk_users_uuid ON users(uuid);
