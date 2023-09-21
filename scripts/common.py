from sys import argv
from pathlib import Path

with open(Path(__file__).parent / "hosts.txt") as hosts_file:
    HOSTS = [
        line
        for line in (line.strip() for line in hosts_file.read().splitlines())
        if line and not line.startswith("#")
    ]

SERVICE_HOST = None
for host in HOSTS:
    if host.startswith("service"):
        HOSTS.remove(host)
        _, SERVICE_HOST = host.split(" ")
        break
SERVICE = f"http://{SERVICE_HOST}:8080"

WORK_DIR = "/home/ubuntu"
# WORK_DIR = "/local/cowsay/artifacts"


NUM_HOST_PEER = 200
NUM_HOST_BENCHMARK_PEER = 1
NUM_TOTAL_PEER = NUM_HOST_PEER * len(HOSTS)

if __name__ == "__main__":
    print(globals()[argv[1]])
