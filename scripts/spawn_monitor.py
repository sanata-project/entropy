import asyncio
import sys
import pathlib

from common import (
    SERVICE as PLAZA,
    NUM_HOST_PEER as NUM_PEER,
    NUM_HOST_BENCHMARK_PEER as NUM_BENCHMARK_PEER,
    FRAGMENT_SIZE,
    INNER_K,
    INNER_N,
    OUTER_K,
    OUTER_N,
    REPAIR_CONCURRENCY,
    PROTOCOL,
)

ARGV = dict(enumerate(sys.argv))
HOST = ARGV.get(1, "10.0.0.1")
EXTRA_ARGS = sys.argv[2:]
WORK_DIR = pathlib.Path(__file__).absolute().parent

if "--repair" in EXTRA_ARGS:
    if PROTOCOL == "entropy":
        OUTER_K = OUTER_N = REPAIR_CONCURRENCY
    if PROTOCOL == "kademlia":
        INNER_K = INNER_N = 1
        OUTER_K = OUTER_N = REPAIR_CONCURRENCY


async def run_peers():
    tasks = []
    for index in range(NUM_PEER):
        command = [
            "RUST_LOG=info",
            "RUST_BACKTRACE=1",
            # "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT=http://10.0.0.1:4317",
            "OTEL_SDK_DISABLED=true",
            f"{WORK_DIR}/entropy",
            HOST,
            "--port",
            10000 + index,
            "--plaza",
            PLAZA,
            "--num-host-peer",
            NUM_PEER,
            "--fragment-size",
            FRAGMENT_SIZE,
            "--inner-k",
            INNER_K,
            "--inner-n",
            INNER_N,
            "--outer-k",
            OUTER_K,
            "--outer-n",
            OUTER_N,
            *EXTRA_ARGS,
        ]
        if index < NUM_BENCHMARK_PEER:
            command.append("--benchmark")
        command += [
            f"1>{WORK_DIR}/entropy-{index:03}-output.txt",
            f"2>{WORK_DIR}/entropy-{index:03}-errors.txt",
        ]
        proc = await asyncio.create_subprocess_shell(
            " ".join(str(item) for item in command)
        )

        async def wait(proc, index):
            code = await proc.wait()
            return code, index

        tasks.append(asyncio.create_task(wait(proc, index)))

    active_shutdown = False
    while tasks:
        done_tasks, tasks = await asyncio.wait(
            tasks, return_when=asyncio.FIRST_COMPLETED
        )
        for done_task in done_tasks:
            code, index = done_task.result()
            if code != 0:
                print(f"peer {index} on {HOST} crashed ({code})")
                if not active_shutdown:
                    asyncio.create_task(shutdown_peers())
                    active_shutdown = True
    return active_shutdown


async def shutdown_peers():
    proc = await asyncio.create_subprocess_shell(f"curl -s -X POST {PLAZA}/shutdown")
    await proc.wait()


async def main():
    # print("run peers")
    if await run_peers():
        exit(1)


if __name__ == "__main__":
    asyncio.run(main())
