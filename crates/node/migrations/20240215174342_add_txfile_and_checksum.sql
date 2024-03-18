-- Add checksum to task files
-- Add a new table to store Tx files.

ALTER TABLE file ADD checksum VARCHAR(64) NOT NULL;

DROP TABLE IF EXISTS txfile;

CREATE TABLE txfile (
    tx_id VARCHAR(64) NOT NULL,
    name VARCHAR(256) NOT NULL,
    url VARCHAR(2048) NOT NULL,
    checksum VARCHAR(64) NOT NULL,
    CONSTRAINT fk_tx
        FOREIGN KEY (tx_id)
             REFERENCES transaction (hash) ON DELETE CASCADE
);
