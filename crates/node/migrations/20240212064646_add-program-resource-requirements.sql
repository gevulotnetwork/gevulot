-- Add migration script here

DROP TABLE IF EXISTS program_resource_requirements;

CREATE TABLE program_resource_requirements (
    program_hash VARCHAR(64) PRIMARY KEY,
    memory  BIGINT NOT NULL,
    cpus BIGINT NOT NULL,
    gpus BIGINT NOT NULL,
    CONSTRAINT fk_program_hash
        FOREIGN KEY (program_hash)
            REFERENCES program (hash) ON DELETE CASCADE
);
