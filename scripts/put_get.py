import asyncio
import aiohttp
import sys
import random

from common import NUM_HOST_BENCHMARK_PEER, HOSTS, SERVICE as PLAZA


ARGV = dict(enumerate(sys.argv))
NUM_OPERATION = int(ARGV.get(1, "1"))
NUM_CONCURRENT = int(ARGV.get(2, "1"))
assert NUM_CONCURRENT <= NUM_OPERATION


def to_timestamp(system_time):
    return (
        system_time["secs_since_epoch"]
        + system_time["nanos_since_epoch"] / 1000 / 1000 / 1000
    )


async def ready():
    async with aiohttp.ClientSession() as session:
        ready = False
        while not ready:
            async with session.get(f"{PLAZA}/ready") as resp:
                ready = await resp.json()


async def put_get(peer):
    async with aiohttp.ClientSession() as session:
        print("commit put operation")
        async with session.post(f"{peer}/benchmark/put") as resp:
            put_id = await resp.json()
        print("poll status")
        while True:
            await asyncio.sleep(1)
            async with session.get(f"{peer}/benchmark/put/{put_id}") as resp:
                result = await resp.json()
                if result["put_end"]:
                    break
        latency = to_timestamp(result["put_end"]) - to_timestamp(result["put_start"])
        print(f",{peer},put,{latency}")
        await asyncio.sleep(1)

        print("commit get operation")
        await session.post(f"{peer}/benchmark/get/{put_id}")
        print("poll status")
        while True:
            await asyncio.sleep(1)
            async with session.get(f"{peer}/benchmark/put/{put_id}") as resp:
                result = await resp.json()
                if result["get_end"]:
                    break
        latency = to_timestamp(result["get_end"]) - to_timestamp(result["get_start"])
        print(f",{peer},get,{latency}")


async def operation(peers):
    await put_get(random.choice(peers))


async def main():
    await ready()
    peers = [
        f"http://{host}:{10000 + index}"
        for host in HOSTS
        for index in range(NUM_HOST_BENCHMARK_PEER)
    ]
    tasks = []
    for _ in range(NUM_CONCURRENT):
        tasks.append(asyncio.create_task(operation(peers)))
    num_operation = NUM_CONCURRENT
    while tasks:
        done_tasks, tasks = await asyncio.wait(
            tasks, return_when=asyncio.FIRST_COMPLETED
        )
        for _ in done_tasks:
            if num_operation < NUM_OPERATION:
                num_operation += 1
                tasks.add(asyncio.create_task(operation()))


if __name__ == "__main__":
    asyncio.run(main())
