-- Add migration script here
CREATE TABLE IF NOT EXISTS key_value (
    key VARCHAR PRIMARY KEY,
    value VARCHAR NOT NULL
);