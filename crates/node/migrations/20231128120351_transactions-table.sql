
DROP TABLE IF EXISTS program_output_data;
DROP TABLE IF EXISTS program_input_data;
DROP TABLE IF EXISTS workflow_step;
DROP TABLE IF EXISTS deploy;
DROP TABLE IF EXISTS transaction;
DROP TYPE IF EXISTS transaction_kind;

CREATE TYPE transaction_kind AS ENUM ('empty','transfer', 'stake', 'unstake', 'deploy', 'run', 'proof', 'proofkey', 'verification', 'cancel');

CREATE TABLE transaction (
    hash VARCHAR(64) PRIMARY KEY NOT NULL,
    kind transaction_kind NOT NULL,
    nonce NUMERIC NOT NULL,
    signature VARCHAR(128) NOT NULL,
    propagated BOOLEAN
);

CREATE TABLE deploy (
    tx VARCHAR(64) PRIMARY KEY NOT NULL,
    name VARCHAR(256),
    prover VARCHAR(64),
    verifier VARCHAR(64),
    CONSTRAINT fk_transaction
        FOREIGN KEY (tx)
            REFERENCES transaction (hash) ON DELETE CASCADE
); 

CREATE TABLE workflow_step (
    id BIGSERIAL UNIQUE,
    tx VARCHAR(64) NOT NULL,
    sequence INTEGER NOT NULL,
    program VARCHAR(64) NOT NULL,
    args VARCHAR(512)[],
    PRIMARY KEY (tx, sequence),
    CONSTRAINT fk_transaction
        FOREIGN KEY (tx)
            REFERENCES transaction (hash) ON DELETE CASCADE,
    CONSTRAINT fk_program
        FOREIGN KEY (program)
            REFERENCES program (hash) ON DELETE CASCADE
); 

CREATE TABLE program_input_data (
    workflow_step_id INTEGER NOT NULL,
    file_name VARCHAR(1024) NOT NULL,
    file_url VARCHAR(4096) NOT NULL,
    checksum VARCHAR(512) NOT NULL,
    PRIMARY KEY (workflow_step_id, file_name),
    CONSTRAINT fk_workflow_step
        FOREIGN KEY (workflow_step_id)
            REFERENCES workflow_step (id) ON DELETE CASCADE
);

CREATE TABLE program_output_data (
    workflow_step_id INTEGER NOT NULL,
    file_name VARCHAR(1024) NOT NULL,
    source_program VARCHAR(64) NOT NULL,
    PRIMARY KEY (workflow_step_id, file_name),
    CONSTRAINT fk_workflow_step
        FOREIGN KEY (workflow_step_id)
            REFERENCES workflow_step (id) ON DELETE CASCADE,
    CONSTRAINT fk_source_program
        FOREIGN KEY (source_program)
            REFERENCES program (hash) ON DELETE CASCADE
);
