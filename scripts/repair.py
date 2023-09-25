import asyncio
import aiohttp
import sys

from common import SERVICE as PLAZA, OUTER_N, INNER_K, OUTER_K

ARGV = dict(enumerate(sys.argv))
PROTOCOL = ARGV.get(1, "entropy")
NUM_OPERATION = int(ARGV.get(2, "1"))
NUM_ROUND = int(ARGV.get(3, "1"))


async def ready():
    proc = await asyncio.create_subprocess_shell(
        f"python3 scripts/put_get.py {PROTOCOL} {NUM_OPERATION}"
    )
    assert await proc.wait() == 0


async def repair(num_round, num_repair):
    async with aiohttp.ClientSession() as session:
        for n in range(num_round):
            print(f"repair round {n}")
            async with session.post(f"{PLAZA}/repair") as resp:
                assert resp.status == 200
            result = 0
            while result < num_repair:
                await asyncio.sleep(1)
                async with session.get(f"{PLAZA}/repair/finish") as resp:
                    result = await resp.json()
            await asyncio.sleep(5)


async def main():
    await ready()
    if PROTOCOL == 'entropy':
        num_repair = NUM_OPERATION * OUTER_N
    if PROTOCOL == 'kademlia':
        num_repair = NUM_OPERATION * INNER_K * OUTER_K
    await repair(NUM_ROUND, num_repair)


if __name__ == "__main__":
    asyncio.run(main())
