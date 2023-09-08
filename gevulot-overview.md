# Gevulot

A permissionless, programmable layer one network for deploying proof systems as on-chain programs. It allows users to deploy and use entire proof systems on-chain, with minimal computational overhead as compared to single prover architectures. The vision of Gevulot is to make the creation of e.g. validity rollups as easy as deploying smart contracts.

## Programs

Gevulot programs come in two varieties: provers and verifiers. A verifier program can be deployed standalone, while a prover program must always be deployed with an accompanying verifier program (referred to jointly as a proof system).

Both types of programs can be written in a variety of languages such as Rust, C, C++, etc, and are compiled into [unikernel](https://en.wikipedia.org/wiki/Unikernel) images. Most open source prover implementations can be compiled into unikernel images with minimal or no modification. Examples for Marlin, ED25519 and Filecoin SNARK provers compiled into unikernel images with performance benchmarks can be found [here](https://github.com/eqlabs/gevulot/blob/main/benchmarks.md).

Supported languages: C, C++, Go, Java, Node.js, Python, Rust, Ruby and PHP.

### Unikernel

Unikernels are very lightweight operating systems designed to run only a single process. Due to their simplicity, unikernels offer a compelling mix of performance and security, allowing Gevulot programs to match  centralised prover implementations in speed, while ensuring effective sandboxing of the software. 

Gevulot uses the [Nanos](https://nanos.org/) unikernel running in a [KVM](https://www.linux-kvm.org/page/Main_Page) hypervisor, which provides the following features:

1. Multi-threading 
2. GPU support 
3. Language support 
4. Efficient orchestration
5. Fast boot times

### Fees

A cycle in Gevulot is a fixed amount of blocks and functions as an objective measure of a programs running time. A user pays fees based on the amount of cycles he wants the nodes to run the prover program. If the user does not pay for enough cycles, the program will not complete and the nodes will return a fail. If the user pays for excessive cycles, the nodes will return the output as soon as the program completes due to the competitive dynamics described in the Network section and the user will only pay for the cycles it took for the fastest prover to complete the proof.

Note: We are exploring the possibility of having continuously running programs, which output periodic proofs.

## Network

The Gevulot network can be seen as having two distinct layers which together converge on network state.

### Blockchain State

This is regular blockchain state, which includes balances, transfers, staking and proof system deployments. This layer functions like most proof of stake blockchains today (except that there is no smart contract state), in that validators stake, there is a leader election, the leader builds a block from mempool transactions and verified proofs, a subset of the rest of the network re-executes the state changes and the leader gets rewarded. We are currently intending to use Narwhal/Bullshark consensus, due to its performance characteristics and structured mempool. 

### Proof System State

The proof system state is only for prover and verifier programs. These programs are not re-executed like in most blockchains. Instead, each proving workload is allocated to a small group of nodes using a Verifiable Random Function (VRF). Verification can be completed by any node until the 1/3 verification threshold is met.

Note: We are not currently planning to offer application state storage in Gevulot. Proofs will be stored indefinitely and so application state can be stored anywhere and verified to be correct. E.g. You could store application state on [Filecoin](https://filecoin.io/) or [Arweave](https://www.arweave.org/).

#### Proving 

In each block new proving workloads are distributed to small groups of provers using a Verifiable Random Function. The user pays for a set amount of cycles (defined as X blocks) and the chosen provers run the program either until the program completes, in which case they return the output, or for the maximum paid for cycles without completing, in which case they return fail.

##### Proving Incentives

There is a proving reward paid by the network, which is paid to provers who complete the workload in the given cycle time. It is tiered based on speed, wherein the first node to return an output receives the majority of the reward, the second a smaller portion and the third a smaller portion still. All nodes who complete the workload in the given cycle time receive at least some portion of the reward. This reward will proportional to the amount of cycles it takes for the first prover to complete the workload. Additionally, if the program completes, the fees paid by the user are distributed amongst all provers in the group which produced an output within the given amount of cycles. If the program does not complete, the fees are burned. 

#### Verifying

The proof is verified by a subset of all network nodes, until the verification threshold has been reached, after which the leader includes it in a block and the prover is rewarded. It is also possible for the verification threshold to have been reached before the leader includes the proof in a block in which case the leader does not need to re-verify. The verification threshold here simply constitutes finality from the network's perspective, but any honest node can have a deterministic guarantee of finality by simply verifying the proof.

### Data

The Gevulot network will potentially need to handle large amounts of data and propagate that data efficiently in the network to maximise performance. We have designed the network with the end-user experience as the foremost consideration and worked our way to a network architecture from there, never compromising on performance from the end-users perspective.

In this section we'll look at a full data lifecycle in the Gevulot network, from the perspective of a user calling a prover program with inputs.

#### Transaction Broadcast

A user that is either running a full node or connected to an RPC provider sends a transaction to the Gevulot network. This transaction is added to the mempool and broadcast to one or more nodes, all of which can choose to execute the prover program associated with that transaction. Once the prover program execution is completed the node will return the output of the proof and the associated verification to the mempool and broadcast to the Gevulot network. 

In some proof systems, the proof will be small and the verification data large, while in others its the other way around. Given this, nodes will need significant bandwidth in order for the network to propagate the data for arbitrary proof systems. Gevulot will use a structured broadcast like [Kadcast](https://eprint.iacr.org/2021/996.pdf) for efficient propagation while minimizing redundancy to acceptable levels.  

Note: Once the proof is in the mempool, the users full node or RPC provider can immediately verify the proof and get a deterministic guarantee of finality.

#### Verification

As proofs get added into the mempool along with the associated verification data, other nodes will receive the data and verify the proof. Nodes only need to verify proofs which are under the verification threshold. If they see a proof with verifications exceeding the threshold, they can immediately remove the verification data and simply store the proof. If they see a proof under the threshold, they verify the proof, broadcast the proof and associated verification data and then store only the proof.

#### Block Propagation

As the leader receives blockchain state transactions and proofs which it either verifies or surpass the verification threshold, it orders them and includes them into blocks. The ordering is then propagated to the network using the same structured broadcast, where other nodes receive the ordering data, execute any blockchain state transitions and order the proofs, thus converging to consensus.

### Rewards

Gevulot utilizes a number of in-protocol reward mechanisms to incentivize participants to act honestly and maximize performance.

Note: The reward structures may change considerably as we hone in on the optimal structure for the network.

#### Block Reward

The block reward in Gevulot functions similarly to a block reward in any other blockchains, wherein the leader, after building a block, receives a reward for doing so. In addition the block builder will get a small reward for every proof they include in a block where the threshold for verifying has been met.

#### Prover Reward

Described in the "Proving Incentives" section.

## Usecases

Validity proofs are a very powerful primitive and so while the Gevulot network architecture is comparatively simple, the proof systems deployed on it can serve an almost unbounded array of usecases. Here we'll explore a few.

### Prover-as-a-service

Modern validity rollups and other services which need to compute proofs can use Gevulot to outsource their proving. Where in many cases projects would need to either bootstrap their own prover networks or rely on their sequencers to manage heavy computational loads, with Gevulot they can simply deploy a proof system program.

### ZkVMs

There are already tens of zkvm implementations including [CairoVM](https://crates.io/crates/cairo-vm), [Risc0](https://www.risczero.com/), [Wasm0](https://github.com/wasm0), [zkBPF](https://github.com/Eclipse-Laboratories-Inc/zk-bpf), [zkEVMs](https://github.com/LuozhuZhang/awesome-zkevm). Any such zkVM implementation can be deployed on Gevulot and used as a standalone environment. 

### Bridges

Zk-bridges such as [Succinct Labs](https://www.succinct.xyz/)' Telepathy rely on Proof-of-Consensus, which can also be deployed to Gevulot as a proof system leading to minimal additional hardware needing to be run.

### Aggregation

Many rollup systems have a need to aggregate a large number of smaller proofs into one aggregate proof. For example a privacy rollup like [Aztec](https://aztec.network/), which needs to aggregate a large number of private proofs into one aggregate proof could use Gevulot for the aggregation while retaining its desired privacy guarantees. 

