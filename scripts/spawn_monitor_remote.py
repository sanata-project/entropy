import asyncio
import sys
import json
from subprocess import run, PIPE

ARGV = dict(enumerate(sys.argv))
WORK_DIR = "/home/ubuntu"
# WORK_DIR = "/local/cowsay/artifacts"
ARTIFACT = "./target/artifact/simple-entropy"
SPAWN_MONITER = "./scripts/spawn_monitor.py"

# HOSTS = ["10.0.0.1", "10.0.0.2", "10.0.0.3", "10.0.0.4"]
# SSH_HOSTS = ["nsl-node1.d2", "nsl-node2.d2", "nsl-node3.d2", "nsl-node4.d2"]
HOSTS = json.loads(
    run(
        "terraform -chdir=terraform output -json hosts",
        shell=True,
        check=True,
        stdout=PIPE,
        text=True,
    ).stdout
)
SSH_HOSTS = HOSTS

PLAZA_HOST = run(
    "terraform -chdir=terraform output -raw service",
    shell=True,
    check=True,
    stdout=PIPE,
    text=True,
).stdout
PLAZA = f"http://{PLAZA_HOST}:8080"


async def upload_artifact():
    tasks = []
    for host in set(SSH_HOSTS):
        proc = await asyncio.create_subprocess_shell(
            f"rsync {ARTIFACT} {host}:{WORK_DIR}/entropy"
        )
        tasks.append(proc.wait())
        proc = await asyncio.create_subprocess_shell(
            f"rsync {SPAWN_MONITER} {host}:{WORK_DIR}/spawn_monitor.py"
        )
        tasks.append(proc.wait())
    codes = await asyncio.gather(*tasks)
    assert all(result == 0 for result in codes)


async def run_remotes():
    tasks = []
    for ssh_host, host in zip(SSH_HOSTS, HOSTS):
        proc = await asyncio.create_subprocess_shell(
            f"ssh -q {ssh_host} python3 {WORK_DIR}/spawn_monitor.py {host} {PLAZA} {WORK_DIR}"
        )
        tasks.append(proc.wait())
    codes = await asyncio.gather(*tasks)
    return any(result != 0 for result in codes)


async def main():
    print("upload artifact")
    await upload_artifact()
    print("run remotes")
    if await run_remotes():
        exit(1)


if __name__ == "__main__":
    asyncio.run(main())
