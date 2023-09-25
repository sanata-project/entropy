import asyncio
import sys

ARGV = dict(enumerate(sys.argv))
REMOTE = ARGV.get(1) == "remote"


async def prepare_remote():
    proc = await asyncio.create_subprocess_shell(
        'echo "* soft nofile 1048576" | sudo tee /etc/security/limits.conf',
        stdout=asyncio.subprocess.DEVNULL,
    )
    assert await proc.wait() == 0
    proc = await asyncio.create_subprocess_shell("sudo ethtool -G ens5 rx 16384")
    assert await proc.wait() == 0


async def prepare(host):
    from common import WORK_DIR

    proc = await asyncio.create_subprocess_shell(f"rsync {__file__} {host}:{WORK_DIR}")
    assert await proc.wait() == 0
    proc = await asyncio.create_subprocess_shell(
        f"ssh {host} python3 {WORK_DIR}/prepare_hosts.py remote"
    )
    assert await proc.wait() == 0


async def main():
    if REMOTE:
        await prepare_remote()
    else:
        from common import HOSTS, SERVICE_HOST

        tasks = []
        for host in set(HOSTS) | {SERVICE_HOST}:
            tasks.append(prepare(host))
        await asyncio.gather(*tasks)


if __name__ == "__main__":
    asyncio.run(main())
