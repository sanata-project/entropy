import asyncio
import aiohttp
import sys

from common import SERVICE as PLAZA, REPAIR_CONCURRENCY, NUM_OPERATION

ARGV = dict(enumerate(sys.argv))
NUM_ROUND = int(ARGV.get(1, "1"))


async def ready():
    proc = await asyncio.create_subprocess_shell(
        f"python3 scripts/put_get.py {NUM_OPERATION}"
    )
    assert await proc.wait() == 0


async def repair(num_round):
    async with aiohttp.ClientSession() as session:
        for n in range(num_round):
            print(f"repair round {n}")
            async with session.post(f"{PLAZA}/repair") as resp:
                assert resp.status == 200
            result = 0
            while result < REPAIR_CONCURRENCY * NUM_OPERATION * (n + 1):
                await asyncio.sleep(1)
                async with session.get(f"{PLAZA}/repair/finish") as resp:
                    result = await resp.json()
            await asyncio.sleep(5)


async def main():
    await ready()
    await repair(NUM_ROUND)


if __name__ == "__main__":
    asyncio.run(main())
