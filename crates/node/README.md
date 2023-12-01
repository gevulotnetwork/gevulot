# Gevulot Node

**Work in Progress**

## Initial setup

Preliminary assumptions:
- Linux operating system
- x86_64 processor
- Rust installed

### Dependencies

- `cuda-opencl-<ver>`
- `cuda-opencl-devel-<ver>`
- `hwlock-devel`
- `ocl-icd-devel`
- `protobuf`
- `protobuf-c`
- `protobuf-compiler`
- `protobuf-devel`

- `cargo install sqlx-cli`

### Gevulot working directory

`mkdir -p /var/lib/gevulot`

Also ensure appropriate rights for that directory.

### Database

#### Initialization
- `podman-compose up`
- `cargo sqlx database create --database-url postgres://gevulot:gevulot@localhost/gevulot`
- `cargo sqlx migrate run --database-url postgres://gevulot:gevulot@localhost/gevulot`

#### Refresh SQLX cache
- `cargo sqlx prepare --database-url postgres://gevulot:gevulot@localhost/gevulot`
