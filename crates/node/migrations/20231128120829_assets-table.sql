
DROP TABLE IF EXISTS assets;

CREATE TABLE assets (
    tx VARCHAR(64) PRIMARY KEY NOT NULL,
    created TIMESTAMP NOT NULL DEFAULT NOW(),
    completed TIMESTAMP,
    CONSTRAINT fk_transaction
        FOREIGN KEY (tx)
            REFERENCES transactions (hash) ON DELETE CASCADE
);
