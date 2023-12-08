
DROP TABLE IF EXISTS program;

CREATE TABLE program (
    hash VARCHAR(64) PRIMARY KEY,
    name VARCHAR(128) NOT NULL,
    image_file_name VARCHAR(256) NOT NULL,
    image_file_url VARCHAR(1024) NOT NULL,
    image_file_checksum VARCHAR(128) NOT NULL
);
