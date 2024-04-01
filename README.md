# Gevulot

Gevulot is a permissioned and programmable layer one blockchain for deploying zero-knowledge provers and verifiers as on-chain programs. It allows users to deploy and use entire proof systems on-chain, with minimal computational overhead as compared to single prover architectures. The vision of Gevulot is to make the creation and operation of zk-based systems, such as validity rollups, as easy as deploying smart contracts.

For a more in-depth look at the network design see our [docs](https://gevulot.gitbook.io/gevulot-docs/).

The current status of the project is pre-alpha.

## Gevulot Node

Gevulot node is written in Rust and packaged into a container. It uses QEMU-KVM as its hypervisor to run unikernel programs.

### Building container

To build Gevulot node container image:

```
podman build -t gevulot-node .
```

### Running the node

In order to run the node, refer [installation guide](INSTALL.md).

### Development

For development you need following dependencies (package names for Fedora):

- `openssl-devel`
- `protobuf`
- `protobuf-c`
- `protobuf-compiler`
- `protobuf-devel`

#### Database

##### Local postgres container under systemd

Local development postgres can be run e.g. as a user's quadlet systemd unit:

**~/.config/containers/systemd/gevulot-postgres.container**
```
[Install]
WantedBy=default.target

[Container]
ContainerName=gevulot-postgres

Image=docker.io/library/postgres:16-alpine

Environment=POSTGRES_USER=gevulot
Environment=POSTGRES_PASSWORD=gevulot
Environment=POSTGRES_DB=gevulot

Network=host
ExposeHostPort=5432
```

##### Initialization

`sqlx-cli` can be run from `crates/node` directory as follows:
- **Create database**:
  - `cargo sqlx database create --database-url postgres://gevulot:gevulot@localhost/gevulot`
- **Run DB migrations**:
  - `cargo sqlx migrate run --database-url postgres://gevulot:gevulot@localhost/gevulot`

##### Refresh SQLX cache
- `cargo sqlx prepare --database-url postgres://gevulot:gevulot@localhost/gevulot`

## License

This library is licensed under either of the following licenses, at your discretion.

[Apache License Version 2.0](LICENSE-APACHE)

[MIT License](LICENSE-MIT)

Any contribution that you submit to this library shall be dual licensed as above (as defined in the Apache v2 License), without any additional terms or conditions.
