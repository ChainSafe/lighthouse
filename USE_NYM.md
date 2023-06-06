# Validator privacy with Nym

This memo describes use Nym mixnet as an alternative transport for Lighthouse Eth2 consensus client. This enhancement aims to provide better anonymity for beacon chain validators by opting for network-level privacy enabled by Nym mixnet.

## Requirements
- Rust >= 1.68
- Build dependencies (see [docs](https://lighthouse-book.sigmaprime.io/installation-source.html#dependencies) for full list)


## Simulation

Run a simulation with TCP transport (for reference):
```bash
cargo run -p simulator --release -- eth1-sim -t tcp --nodes 3 --validators_per_node 1
```

Run a simulation with Nym transport:
```bash
cargo run -p simulator --release -- eth1-sim -t nym --nodes 3 --validators_per_node 1
```

To monitore mentrics (e.g. block latency) see instruction in [lighthouse-metrics](https://github.com/ChainSafe/lighthouse-metrics/tree/nym) repo. Use `nym` branch to access Nym metrics dashboard.

## Usage 

### Setup local Nym WebSocket client

First obtain Nym binaries. You can chose to build them from source (instruction [here](https://nymtech.net/docs/binaries/building-nym.html#download-and-build-nym-binaries)) or download the latest released ones from [GitHub](https://github.com/nymtech/nym/releases).

When building from source the needed binary will be placed in `./target/release` folder in the location where you cloned Nym repo. The binary of interest is `nym-client`.

What's left is to initialise Nym client as following:

```bash
./target/release/nym-client init --id eth-validator
```

And run it:

```bash
./target/release/nym-client run --id eth-validator
```

> **Note**: Full documentation of Nym WebSocket client is available [here](https://nymtech.net/docs/clients/websocket-client.html).

### Install modified Lighthouse node

This Nym-enabled Lighthouse fork requires same set of dependencies as the stable branch, so make sure you have everything installed as specified in [documentation](https://lighthouse-book.sigmaprime.io/installation-source.html#dependencies). 

Once you have build dependencies you're ready to build Lighthouse:

```bash
git clone https://github.com/ChainSafe/lighthouse && cd lighthouse
```

```bash
git checkout nym
```

```bash
make
```

Installation was successful if `lighthouse --help` displays the command-line documentation.

### Run beacon node behind Nym mixnet

> **Note**: Make sure you have set up an execution node as described in documentation [here](https://lighthouse-book.sigmaprime.io/run_a_node.htm).

To enable stronger anonymity for your local beacon chain node we are going to configure it to gossip proposed blocks through the Nym mixnet. This is in contrast to the default configuration that uses a regular TCP transport and is susceptible to metadata leakage. To switch to Nym transport `--transport nym` command line argument must be used.

Internally this is enabled with [`ChainSafe/rust-libp2p-nym`](https://github.com/ChainSafe/rust-libp2p-nym) module that will tunnel all the outgoing and ingoing packets through Nym Websocket client that we started before. By default Lighthouse will use `ws://127.0.0.1:1977` address but it is possible to specify custom one using `NYM_CLIENT` env variable.

> **Note**: Each beacon node must be connected to a unique Nym client.

For Nym-enabled node to gossip block over mixnet it must know at least one other  node that supports `nym` or `nym-either-tcp` transports. Currently automatic discovery of Nym-enabled nodes isn't available so you would need to specify them manually using `--enr-address` argument.

When starting Lighthouse beacon node specify this additional command line arguments as shown below:

```bash
lighthouse bn \
 # ... standart arguments as described in Lighthouse documentation
 --network goerli \ 
 --transport nym
 --enr-address ... # ENR addresses of nodes that support `nym` or `nym-either-tcp` transports
```

### Networks

> **Warning**: This branch is a proof of concept software. It is advised to used it on testnet networks only.
