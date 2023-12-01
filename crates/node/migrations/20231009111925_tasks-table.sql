
DROP TABLE IF EXISTS files;
DROP TABLE IF EXISTS tasks;
DROP TYPE IF EXISTS task_state;
DROP TYPE IF EXISTS task_kind;

CREATE TYPE task_state AS ENUM ('new', 'pending', 'running', 'ready', 'failed');
CREATE TYPE task_kind AS ENUM ('proof', 'verification', 'pow', 'nop');

CREATE TABLE tasks (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    name VARCHAR(128) NOT NULL,
    args VARCHAR(512)[],
    kind task_kind NOT NULL DEFAULT 'nop',
    program_id VARCHAR(64) NOT NULL,
    serial SERIAL,
    state task_state,
    CONSTRAINT fk_program
        FOREIGN KEY (program_id)
            REFERENCES programs (hash) ON DELETE CASCADE
);

CREATE TABLE files (
    task_id uuid NOT NULL,
    name VARCHAR(256) NOT NULL,
    url VARCHAR(2048) NOT NULL,
    CONSTRAINT fk_task
        FOREIGN KEY (task_id)
             REFERENCES tasks (id) ON DELETE CASCADE
);
