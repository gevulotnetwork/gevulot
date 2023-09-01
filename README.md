# Gevulot

Gevulot is a permissionless and programmable layer one blockchain for deploying zero-knowledge provers and verifiers as on-chain programs. It allows users to deploy and use entire proof systems on-chain, with minimal computational overhead as compared to single prover architectures. The vision of Gevulot is to make the creation of e.g. validity rollups as easy as deploying smart contracts.

For a more in-depth look at the network design see [overview](https://github.com/gevulotnetwork/gevulot/blob/main/gevulot-overview.md).

The current status of the project is pre-alpha.

## Unikernel Setup

Gevulot provers run in a [Nanos unikernel](https://nanovms.com/). Each prover must be a single binary that performs the proof computation. The binary is packaged with an execution manifest and required dynamic libraries into an image file that is then run to produce a proof.

### Tooling

[Ops](https://ops.city/) provides functionality to prepare unikernel images, disk volumes and execute instances. It supports local execution under QEMU/KVM hypervisors and various cloud providers.

### Running a Prover in a Unikernel

Running a prover in a unikernel is  quite simple:

- [Ensure that Ops is installed](https://docs.ops.city/ops/getting_started#installing-ops)
- Build Linux x86_64 binary.
- If the application loads dynamic libraries during runtime (not listed in ELF headers), then bundle these under `lib64` directory in unikernel image.
- [Prepare execution manifest](https://docs.ops.city/ops/configuration)
- [Google Cloud: Build the unikernel image](https://docs.ops.city/ops/google_cloud#create-image)
- [Build auxiliary disk volume, if needed](https://docs.ops.city/ops/volumes)
- Locally: `ops run <binary> -c config.json [--mounts volume:/directory]`
- [GCP: Create instance from the image](https://docs.ops.city/ops/google_cloud#create-instance)

#### Debugging

When there are problems with the unikernel execution, for example if the cloud instance stops almost immediately after start, the most common problem comes from dynamic libraries that are loaded during the runtime via `dlopen(3)`.
Finding those files can be done using `strace` when running the program natively on Linux:
```
$ strace -o strace.log -f -e trace=file <binary> <params>
```

...and then looking at `openat(2)` calls for libraries that **are not** present in `ldd <binary>` output.

Another way of debugging unikernel execution is to export [trace log](https://docs.ops.city/ops/debugging#tracing) from `ops run`:
```
$ ops run <binary> [-c myconfig.json] [--mounts myvolume:/mnt] --trace &> trace.log
```

That will produce Nanos' trace log into `trace.log` for further analysis.


### Running Gevulot multi-prover in a Unikernel

#### Compile `prover`

```
$ cd prover
$ cargo build --release
```

#### Generate ED25519 Groth16 Proof & Verify it

First, due to large size of R1CS constraints & witnesses, the proof input data must be uncompressed:
```
$ unzip -d test-data test-data/ed25519.zip
```

Then, create working volume:
```
$ ops volume create deployments -n -s 2g -d deployments
```

...and finally generate the proof & verify it:
```
$ # First the proof:
$ ops run target/release/prover -n -c scripts/ed25519-groth-proof.json --mounts deployments:/deployments
$
$ # Then verification:
$ ops run target/release/prover -n -c scripts/ed25519-groth-verify.json --mounts deployments:/deployments
```

To cleanup:
```
$ ops volume delete deployments
```

#### Generate Sudoku Marlin Proof & Verify it

First, [setup Circom & generate test circuit.](prover/circom/README.md)

Then, create working volume:
```
$ ops volume create deployments -n -s 2g -d deployments
```

...and finally generate the proof & verify it:
```
$ # First the proof:
$ ops run target/release/prover -n -c scripts/sudoku-marlin-proof.json --mounts deployments:/deployments
$
$ # Then verification:
$ ops run target/release/prover -n -c scripts/sudoku-marlin-verify.json --mounts deployments:/deployments
```

To cleanup:
```
$ ops volume delete deployments
```

### Wrapping a generic prover in a unikernel

#### rust-fil-proofs / benchy

Filecoin Proving Subsystem provides a convenient benchmarking tool to compute various proofs.
This is a good test prover for the platform.

##### Prepare Unikernel Image

**NOTE:** Following scripts use `2KiB` sector-size for proof construction. Also `8MiB`, `512MiB`, `32GiB` and `64GiB` are supported, when volume sizes are adjusted accordingly.

*Clone the repo & build benchy:*
```
$ git clone git@github.com:filecoin-project/rust-fil-proofs.git
$ cd rust-fil-proofs
$ cargo build --release

# Create separate directory for the deployment
$ mkdir deployment
$ cp target/release/benchy deployment
$ cp rust-fil-proofs.config.toml.sample deployment/rust-fil-proofs.config.toml
$ cd deployment
```

*Edit `rust-fil-proofs.config.toml` file locations to use base path `/tmp` instead of `/var/tmp`. It should look like following:*
```
# To use this configuration, copy this file to './rust-fil-proofs.config.toml'.

# The location to store downloaded parameter files required for proofs.
parameter_cache = "/tmp/filecoin-proofs-parameters/"

# The location to store the on-disk parents cache.
parent_cache = "/tmp/filecoin-parents"
# The max number of parent cache elements to have mapped in RAM at a time.
sdr_parents_cache_size = 2_048

# This enables the use of the GPU for column tree building.
use_gpu_column_builder = false
# If the GPU is used for column building, this is the batch size to send to the GPU at a time.
max_gpu_column_batch_size = 400_000
# This is the batch size for writing out the column tree elements to disk after it's generated.
column_write_batch_size = 262_144

# This enables the use of the GPU for tree r last building.
use_gpu_tree_builder = false
# If the GPU is used for tree r last building, this is the batch size to send to the GPU at a time.
max_gpu_tree_batch_size = 700_000

# This setting affects tree_r_last (MerkleTree) generation and access
# and determines the size of the on disk tree caches.  This value MUST
# NOT be changed after tree_r_last caches have been generated on your
# system, as any remaining will no longer be accessible.  A tool
# exists called 'update_tree_r_last' that can rebuild cache files if
# it's required, but updating this setting is NOT recommended.
rows_to_discard = 2

# This value is defaulted to the number of cores available on your system.
#window_post_synthesis_num_cpus = 8

# This enables multicore SDR replication
use_multicore_sdr = false
```

*Download param files (adjust sector-size accordingly!):*
```
$ mkdir -p tmp/filecoin-proofs-parameters
$ jq -r 'map_values(select(.sector_size == 2048))| keys[]' ../parameters.json | xargs -I{} curl -L -o tmp/filecoin-proofs-parameters/{} https://proofs.filecoin.io/{}
```

*Create Nanos manifest*

Following one is for local QEMU/KVM use. Save it to `local.json`:
```
{
  "RebootOnExit": true,
  "Files":["rust-fil-proofs.config.toml"],
  "Env":{
    "BELLMAN_NO_GPU": "1",
    "RUST_BACKTRACE": "1",
    "RUST_LOG": "trace"
  },
  "Args":["winning-post", "--size", "2KiB", "--fake"],
  "Program":"benchy"
}
```

*Build the volume with parameter files & space for working files:*
```
$ ops volume create tmp -n -s 40g -d tmp
```

##### Run the unikernel locally

*When running single instances of the unikernel locally, one can omit the image building phase and directly run it:*
```
$ ops run benchy -n -c local.json --mounts tmp:/tmp
```

##### Run the unikernel in Google Cloud

When running unikernels in Google Cloud, the images must be built ahead of time, instances scheduled separately for running them and volumes mounted once the instance is running.

*Prepare `gcloud.json` configuration:*
```
{
  "CloudConfig" :{
    "ProjectID": "<insert your GCP project ID>",
    "Zone": "europe-west1-b",
    "BucketName":"<insert your GCP bucket name>",
    "Flavor":"n1-standard-8"
  },
  "Klibs":["gcp", "tls"],
  "RebootOnExit": true,
  "RunConfig": {
        "CPUs":8
  },
  "Files":["rust-fil-proofs.config.toml"],
  "Env":{
    "BELLMAN_NO_GPU": "1",
    "RUST_BACKTRACE": "1",
    "RUST_LOG": "trace"
  },
  "Args":["winning-post", "--size", "2KiB", "--fake"],
  "Program":"benchy"
}
```

*Create volume in GCP:*
```
$ ops volume create tmp -s 40g -n -t gcp -c gcloud.json -d tmp
```

*Create unikernel image to GCP:*
```
$ ops image create benchy -n -t gcp -c gcloud.json --mounts tmp:/tmp
```

*Start an instance in GCP:*
```
$ ops instance create benchy -t gcp -c gcloud.json

```

Capture the created `instance ID` from the output of previous command. It's `benchy-<timestamp>`, like: `benchy-1693468118`.

*Attach `tmp` volume to running instance:*
```
$ ops volume attach <instance ID> tmp -t gcp -c gcloud.json
```

*Inspect console logs from a running instance:*
```
$ ops instance logs <instance ID> -t gcp -c gcloud.json
```

*Finally, to delete the instance & volume:*
```
$ ops instance delete <instance ID> -t gcp -c gcloud.json
$ ops volume delete tmp -t gcp -c gcloud.json
```


Please note that by default the instance will stop immediately after program completion, so when running in cloud you may want to put some sleep in the end of execution to keep the console logs available:
```
diff --git a/fil-proofs-tooling/src/bin/benchy/main.rs b/fil-proofs-tooling/src/bin/benchy/main.rs
index a3c03b7d..25385b31 100644
--- a/fil-proofs-tooling/src/bin/benchy/main.rs
+++ b/fil-proofs-tooling/src/bin/benchy/main.rs
@@ -3,6 +3,7 @@
 
 use std::io::{stdin, stdout};
 use std::str::FromStr;
+use std::{thread, time};
 
 use anyhow::Result;
 use byte_unit::Byte;
@@ -322,5 +323,7 @@ fn main() -> Result<()> {
         _ => unreachable!(),
     }
 
+    thread::sleep(time::Duration::from_secs(240));
+
     Ok(())
 }
```


## License

This library is licensed under either of the following licenses, at your discretion.

[Apache License Version 2.0](https://github.com/arkworks-rs/marlin/blob/master/LICENSE-APACHE)

[MIT License](https://github.com/arkworks-rs/marlin/blob/master/LICENSE-MIT)

Unless you explicitly state otherwise, any contribution that you submit to this library shall be dual licensed as above (as defined in the Apache v2 License), without any additional terms or conditions.
