from sys import argv

with open("./hosts.txt") as hosts_file:
    HOSTS = [
        line
        for line in (line.strip() for line in hosts_file.read().splitlines())
        if line and not line.startswith("#")
    ]

SERVICE_HOST = "nsl-node1.d2"
# SERVICE_HOST = run(
#     "terraform -chdir=terraform output -raw service",
#     shell=True,
#     check=True,
#     stdout=PIPE,
#     text=True,
# ).stdout
SERVICE = f"http://{SERVICE_HOST}:8080"

# WORK_DIR = "/home/ubuntu"
WORK_DIR = "/local/cowsay/artifacts"


NUM_HOST_PEER = 100
NUM_HOST_BENCHMARK_PEER = 1

if __name__ == "__main__":
    print(globals()[argv[1]])
