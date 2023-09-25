import asyncio
import sys
import re

from common import HOSTS, WORK_DIR

ARGV = dict(enumerate(sys.argv))
ARTIFACT = "./target/artifact/entropy"
SCRIPT_SPAWN_MONITER = "./scripts/spawn_monitor.py"
SCRIPT_COMMON = "./scripts/common.py"
HOSTS_TXT = "./scripts/hosts.txt"
EXTRA_ARGS = sys.argv[1:]


async def upload_artifact():
    tasks = []
    for host in set(HOSTS):
        proc = await asyncio.create_subprocess_shell(
            f"ssh {host} rm -r {WORK_DIR}/entropy_chunk",
            stderr=asyncio.subprocess.DEVNULL,
        )
        tasks.append(proc.wait())
    await asyncio.gather(*tasks)

    tasks = []
    for host in set(HOSTS):
        for path in (ARTIFACT, SCRIPT_SPAWN_MONITER, SCRIPT_COMMON, HOSTS_TXT):
            proc = await asyncio.create_subprocess_shell(
                f"rsync {path} {host}:{WORK_DIR}"
            )
            tasks.append(proc.wait())
    codes = await asyncio.gather(*tasks)
    assert all(result == 0 for result in codes)


async def run_remotes():
    tasks = []
    for host in HOSTS:
        # if match := re.match(
        #     r"ec2-(\d+)-(\d+)-(\d+)-(\d+)\.\w+-\w+-\d\.compute\.amazonaws\.com", host
        # ):
        #     ssh_host = host
        #     host = f"{match[1]}.{match[2]}.{match[3]}.{match[4]}"
        proc = await asyncio.create_subprocess_shell(
            " ".join(
                [f"ssh {host} python3 {WORK_DIR}/spawn_monitor.py {host}", *EXTRA_ARGS]
            )
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
