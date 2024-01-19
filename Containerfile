FROM fedora:39

# Install QEMU
RUN dnf install -y qemu-kvm qemu-img

# Install Ops.
RUN /bin/curl -Lo /bin/ops https://storage.googleapis.com/cli/linux/ops && chmod 755 /bin/ops && /bin/ops update

# Add Gevulot node bin.
ADD target/release/gevulot /gevulot

ADD crates/node/migrations /migrations

RUN mkdir -p /var/lib/gevulot
RUN /gevulot generate node-key

CMD ["run"]
ENTRYPOINT ["/gevulot"]
