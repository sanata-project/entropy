#### Preliminary Starting Steps

Prepare third-party dependencies.

```
$ git clone https://github.com/catid/wirehair wirehair/wirehair
```

Create `scripts/hosts.txt` with following content

```
#service <SERVICE_HOST>
<HOST1>
<HOST2>
...
```

Build artifact and run plaza service
```
$ bash -ex scripts/run_service.sh
```

In the second terminal, spawn and monitor peers on hosts
```
$ python3 scripts/spawn_monitor_remote.py
```

In the third terminal, run put/get benchmark
```
$ python3 scripts/put_get.py [NUM_OPERATION] [NUM_CONCURRENT]
```

Shutdown peers and plaza service
```
$ bash -ex scripts/stop_peers.sh
```

The first two terminals should exit automatically.
