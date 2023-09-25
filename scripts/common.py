from sys import argv
from pathlib import Path

with open(Path(__file__).parent / "hosts.txt") as hosts_file:
    lines = [line.strip() for line in hosts_file.read().splitlines()]
HOSTS = [line for line in lines if line and not line.startswith("#")]

SERVICE_HOST = None
for line in lines:
    if line.startswith("#service"):
        _, SERVICE_HOST = line.split()
SERVICE = f"http://{SERVICE_HOST}:8080"

WORK_DIR = "/home/ubuntu"
# WORK_DIR = "/local/cowsay/artifacts"


NUM_HOST_PEER = 100
NUM_HOST_BENCHMARK_PEER = 1
NUM_TOTAL_PEER = NUM_HOST_PEER * len(HOSTS)

INNER_K = 32
INNER_N = 80
OUTER_K = 8
OUTER_N = 10
FRAGMENT_SIZE = int((1 << 30) / INNER_K / OUTER_K)
# INNER_K = 1
# INNER_N = 1
# OUTER_K = 2
# OUTER_N = 2
# FRAGMENT_SIZE = 4 << 20

# PROTOCOL = "entropy"
PROTOCOL = "kademlia"
REPAIR_CONCURRENCY = 2
assert REPAIR_CONCURRENCY >= 2
if PROTOCOL == "entropy":
    assert REPAIR_CONCURRENCY <= OUTER_K
NUM_OPERATION = 10

if __name__ == "__main__":
    print(globals()[argv[1]])
