import asyncio
import sys

from common import HOSTS, WORK_DIR

ARGV = dict(enumerate(sys.argv))
ARTIFACT = "./target/artifact/entropy"
SCRIPT_SPAWN_MONITER = "./scripts/spawn_monitor.py"
SCRIPT_COMMON = "./scripts/common.py"
HOSTS = "./hosts.txt"


async def upload_artifact():
    tasks = []
    for host in set(HOSTS):
        for path in (ARTIFACT, SCRIPT_SPAWN_MONITER, SCRIPT_COMMON, HOSTS):
            proc = await asyncio.create_subprocess_shell(
                f"rsync {path} {host}:{WORK_DIR}"
            )
            tasks.append(proc.wait())
    codes = await asyncio.gather(*tasks)
    assert all(result == 0 for result in codes)


async def run_remotes():
    tasks = []
    for host in HOSTS:
        await asyncio.sleep(1)
        print(f"spawn monitor on {host}")
        proc = await asyncio.create_subprocess_shell(
            f"ssh {host} python3 {WORK_DIR}/spawn_monitor.py {host}"
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
