from subprocess import run, PIPE
from sys import argv
from json import loads

SERVICE_HOST = run(
    "terraform -chdir=terraform output -raw service",
    shell=True,
    check=True,
    stdout=PIPE,
    text=True,
).stdout
SERVICE = f"http://{SERVICE_HOST}:8080"

HOSTS = loads(
    run(
        "terraform -chdir=terraform output -json hosts",
        shell=True,
        check=True,
        stdout=PIPE,
        text=True,
    ).stdout
)
SSH_HOSTS = HOSTS

# HOSTS = ["10.0.0.1", "10.0.0.2", "10.0.0.3", "10.0.0.4"]
# SSH_HOSTS = ["nsl-node1.d2", "nsl-node2.d2", "nsl-node3.d2", "nsl-node4.d2"]


NUM_HOST_PEER = 100
NUM_HOST_BENCHMARK_PEER = 1

if __name__ == "__main__":
    if argv[1] == "service":
        print(SERVICE)
    elif argv[1] == "service-host":
        print(SERVICE_HOST)
    elif argv[1] == "num-peer":
        print(NUM_HOST_PEER * len(HOSTS))
    else:
        raise ValueError(argv[1])
