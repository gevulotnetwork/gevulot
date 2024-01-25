FROM rust:1-bookworm

COPY ./Cargo.* ./
COPY ./crates ./crates

RUN apt-get update && apt-get upgrade -y && apt-get install -y \
  libssl-dev \
  protobuf-compiler

RUN cargo build --release

FROM debian:bookworm

# Copy Gevulot node bin from earlier build step.
COPY --from=0 target/release/gevulot /gevulot

# Install QEMU.
RUN apt-get update && apt-get upgrade -y && apt-get install -y --no-install-recommends \
  ca-certificates \
  curl \
  qemu-system

# Install Ops.
RUN /bin/curl -Lo /bin/ops https://storage.googleapis.com/cli/linux/ops && chmod 755 /bin/ops && /bin/ops update

COPY ./crates/node/migrations /migrations

RUN mkdir -p /var/lib/gevulot
RUN /gevulot generate node-key

CMD ["run"]
ENTRYPOINT ["/gevulot"]
