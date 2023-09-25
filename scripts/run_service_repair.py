import asyncio
import sys

from common import (
    INNER_K,
    INNER_N,
    OUTER_K,
    OUTER_N,
    REPAIR_CONCURRENCY,
    PROTOCOL,
    NUM_OPERATION,
    NUM_TOTAL_PEER,
)

ARGV = dict(enumerate(sys.argv))
num_concurrency = REPAIR_CONCURRENCY * NUM_OPERATION


async def run_service():
    proc = await asyncio.create_subprocess_shell(
        "bash -ex scripts/run_service.sh", stdout=asyncio.subprocess.PIPE
    )
    while True:
        line = await proc.stdout.readline()
        if not line:
            break
        line = line.decode().rstrip()
        if line.startswith(","):
            print(
                line
                + f",{PROTOCOL},{INNER_K},{INNER_N},{OUTER_K},{OUTER_N},{num_concurrency},{NUM_TOTAL_PEER}"
            )
        else:
            print(line)
    assert await proc.wait() == 0


if __name__ == "__main__":
    print(
        "comment,key,operation,latency,protocol,inner_k,inner_n,outer_k,outer_n,num_concurrent,num_participant"
    )
    asyncio.run(run_service())
