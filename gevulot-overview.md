# Gevulot

Gevulot is a permissionless, programmable layer one blockchain for deploying proof systems as on-chain programs. It allows users to deploy and use entire proof systems on-chain, with minimal computational overhead as compared to single prover architectures. The vision of Gevulot is to make the creation of zk-based systems, such as validity rollups, as easy as deploying smart contracts.

## Programs

Gevulot programs come in two varieties: provers and verifiers. A verifier program can be deployed standalone, while a prover program must always be deployed with an accompanying verifier program (referred to jointly as a proof system). There are no hard constraints on what gevulot programs need to contain, besides that the prover program must output a proof that can be verified by the verifier program for the system to be useful. 

Both types of programs can be written in a variety of languages such as Rust, C, C++, etc, and are compiled into [unikernel](https://en.wikipedia.org/wiki/Unikernel) images. Most open source prover implementations can be compiled into unikernel images with minimal or no modification. Examples for Marlin, ED25519 and Filecoin SNARK provers can be found [here](https://github.com/gevulotnetwork/gevulot/tree/main/prover).

Supported languages: C, C++, Go, Java, Node.js, Python, Rust, Ruby and PHP.

### Unikernel

Unikernels are very lightweight operating systems designed to run only a single process. Due to their simplicity, unikernels offer a compelling mix of performance and security, allowing Gevulot programs to match centralised prover implementations in speed, while ensuring effective sandboxing of the software. 

Gevulot uses the [Nanos](https://nanos.org/) unikernel running in a [KVM](https://www.linux-kvm.org/page/Main_Page) hypervisor, which provides the following features:

1. Multi-threading 
2. GPU support 
3. Language support 
4. Efficient orchestration
5. Fast boot times

### Fees

Gevulot incorporates a fixed fee per "cycle". A cycle in Gevulot is equal to one block and functions as an objective measure of a programs running time. In running a Gevulot program, the user decides how many cycles they want the prover program to run for and how many provers should do so simultaneously. The maximum fee for the user then is:

```
Prover Amount * Cycle Amount
```

If the user does not pay for enough cycles, the program will not complete and the nodes will return a fail. If the user pays for excessive cycles, the nodes will return the output as soon as the program completes and the user will only pay for the cycles it took for the fastest prover to complete the proof.

Note: We are exploring the possibility of having continuously running programs, which output periodic proofs.

## Network

The Gevulot network can be seen as having two distinct node types which together converge on network state: validators and provers. On a high level provers complete proving workloads and validators verify and order proofs into blocks.

Note: We anticipate more node types in the future, such as non-validating full nodes, which just verify proofs and re-execute replicated state transitions.

Note: We are not currently planning to offer application state storage in Gevulot. Proofs will be stored indefinitely and so application state can be stored anywhere and verified to be correct. E.g. You could store application state on [Filecoin](https://filecoin.io/) or [Arweave](https://www.arweave.org/).

### Validators

Validators are responsible for running verifier programs and coming to consensus on replicated state, such as balances, transfers, staking, prover set, prover rewards and proof system deployments. Gevulot avoids replicated state wherever possible, allowing for exceptional performance without introducing onerous hardware requirements.

#### Verification

All proofs are verified by 2/3 of network validators before they are included in a block by the leader.  The verification threshold here simply constitutes finality from the network's perspective, but any honest node can have a deterministic guarantee of finality by simply verifying the proof.

#### Validator Incentives

Validators are rewarded via a traditional block reward and through a small transaction fee paid by the user. This transaction fee is distinct from the concept of cycle fees, which pay for prover compute time and its primary role is to prevent spam.

### Provers

Prover nodes complete proving workloads. To join the active prover set, prover nodes must be staked and must provide information on their hardware capabilities. This information is used to allocate proving workloads to provers with the prerequisite hardware to complete a workload. 

#### Proving 

Each proving workload is allocated to one or more prover nodes using a verifiable random function (VRF) in a deterministic manner, so that the prover for a given workload can be calculated asynchronously. 

The user pays for a set amount of cycles per prover and the chosen provers run the program either until the program completes, in which case they return the output, or for the maximum paid for cycles without completing, in which case they return fail.

#### Prover Incentives

There is a prover reward paid by the network, which is paid to provers who complete the workload in the given cycle time. It is tiered based on speed, wherein the first node to return an output receives the majority of the reward, the second a smaller portion and the third a smaller portion still. All nodes who complete the workload in the given cycle time receive at least some portion of the reward, in addition to their portion of the fees paid by the user. If the program does not complete, the fees are burned. 

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

