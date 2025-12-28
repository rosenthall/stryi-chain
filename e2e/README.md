# End-to-End tests for Stryi Chain

StryiChain has a single e2e test, and the test is very powerful.
It tests the whole system from genesis to the end.
The scenario looks like this:
1. Run chaingen binary to generate a history of 250 deterministic blocks.
2. Root node bootstrap. This node will validate and apply all blocks from genesis to the 250th. It also will be serving as a rendezvous protocol server for the network.
    We use docker compose healthcheck to check that the node is ready:
    After the node starts, we wait ~10 seconds for node to be ready,
    So we can check both http and grpc APIs of the node, using `check-node-state.bash` script. 
3. Set up two client nodes. They will connect to the root node and perform an IBD (initial-block download) operation.
4. Check that these nodes state is identical.
5. todo...


### Scripts
During the e2e test, we use several scripts to validate the state of the system:

- check-node-state.bash – checks node health
    Performs grpc request and http health checks and checks that node is initialized/synced as expected
    State:
    For HTTP it uses curl and jq and `/api/nodestate` endpoint.
    For GRPC it uses grpcurl for `ChainInfo` requests.
    
    Takes four arguments: node address, http port, grpc port, expected tip block hash.
        Example usage: `./check-node-health.bash --address 127.0.0.1 --http 2080 --grpc 2090 --expected-tip Bx93cbe4e59b89e710f0807a62c48b6c21d06154ffa4d5c9a8e3917583a609628c0`


TODO: Finish the stryi_devkit


## How to run locally
```bash
./e2e/scripts/run-e2e.sh
```