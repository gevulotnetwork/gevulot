#!/bin/bash

set -e

# This is the default parameter location.
OUTPUT_DIR="/var/tmp/filecoin-proof-parameters"

# A list of 2KiB parameter files required for tests.
PARAMETER_FILES="v28-proof-of-spacetime-fallback-merkletree-poseidon_hasher-8-0-0-0cfb4f178bbb71cf2ecfcd42accce558b27199ab4fb59cb78f2483fe21ef36d9.params v28-proof-of-spacetime-fallback-merkletree-poseidon_hasher-8-0-0-0cfb4f178bbb71cf2ecfcd42accce558b27199ab4fb59cb78f2483fe21ef36d9.vk v28-proof-of-spacetime-fallback-merkletree-poseidon_hasher-8-0-0-7d739b8cf60f1b0709eeebee7730e297683552e4b69cab6984ec0285663c5781.params v28-proof-of-spacetime-fallback-merkletree-poseidon_hasher-8-0-0-7d739b8cf60f1b0709eeebee7730e297683552e4b69cab6984ec0285663c5781.vk v28-stacked-proof-of-replication-merkletree-poseidon_hasher-8-0-0-sha256_hasher-6babf46ce344ae495d558e7770a585b2382d54f225af8ed0397b8be7c3fcd472.params v28-stacked-proof-of-replication-merkletree-poseidon_hasher-8-0-0-sha256_hasher-6babf46ce344ae495d558e7770a585b2382d54f225af8ed0397b8be7c3fcd472.vk"

# Make sure output location exists.
if [ ! -d ${OUTPUT_DIR} ]; then
    mkdir -p ${OUTPUT_DIR}
fi

# Download parameter files.
for f in ${PARAMETER_FILES}; do
    echo "Checking for ${OUTPUT_DIR}/$f"
    if [ ! -f ${OUTPUT_DIR}/$f ]; then
        echo "Downloading $f"
        wget https://proofs.filecoin.io/$f -O ${OUTPUT_DIR}/$f
    else
        echo "${OUTPUT_DIR}/$f already exists!"
    fi
done