
DROP TABLE IF EXISTS transactions;
DROP TYPE IF EXISTS transaction_kind;

CREATE TYPE transaction_kind AS ENUM ('empty','transfer', 'stake', 'unstake', 'deploy', 'run', 'proof', 'proofkey', 'verification', 'cancel');

CREATE TABLE transactions (
    hash VARCHAR(64) PRIMARY KEY NOT NULL,
    kind transaction_kind NOT NULL,
    propagated BOOLEAN
);
