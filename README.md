#### Preliminary Starting Steps

Prepare third-party dependencies.

```
$ git clone https://github.com/catid/wirehair wirehair/wirehair
```

Build artifact
```
$ cargo build --profile artifact --bin simple-entropy
```

Run the plaza service (to be updated)
```
$ RUST_LOG=info \
    cargo run --release -- <HOST_NAME> --plaza-service <N>
```

Replace `<HOST_NAME>` with server's publicly-known host name, e.g. `localhost`.
Replace `<N>` with the number of peers to join the network.

Modify `scripts/spawn_monitor.py` with expected `WORK_DIR`, `NUM_PEER` for 
number of peers on each host, and `PLAZA` for plaza service endpoint, which
should be `http://<PLAZA_HOST_NAME>:8080`.
Modify `scripts/spawn_monitor_remote.py` with expected `WORK_DIR` and `HOSTS`/
`SSH_HOSTS` for hosts.

Then spawn and monitor peers on hosts
```
$ python3 scripts/spawn_monitor/remote.py
```

Get a list of all peer URI's
```
$ curl http://<PLAZE_HOST_NAME>:8080/run | jq -r .Ready.participants[].Peer.uri
```

Perform a PUT benchmark on a peer
```
$ curl -X POST http://<PEER_URI>/benchmark/put
```

Poll benchmark status
```
$ curl http://<PEER_URI>/benchmark/put | jq
```

Shutdown peers and plaza service
```
$ curl -X POST http://<PLAZE_HOST_NAME>:8080/shutdown
```
