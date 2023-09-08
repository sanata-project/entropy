import asyncio
import sys

ARGV = dict(enumerate(sys.argv))
HOST = ARGV.get(1, "10.0.0.1")
NUM_PEER = 100
WORK_DIR = "/local/cowsay/artifacts"
PLAZA = "http://nsl-node1.d2:8080"


async def run_peers():
    tasks = []
    for index in range(NUM_PEER):
        proc = await asyncio.create_subprocess_shell(
            "RUST_LOG=info RUST_BACKTRACE=1"
            " OTEL_EXPORTER_OTLP_TRACES_ENDPOINT=http://nsl-node1.d2:4317"
            f" {WORK_DIR}/entropy {HOST} --plaza {PLAZA}"
            f" 1> {WORK_DIR}/entropy-{index:03}-output.txt"
            f" 2> {WORK_DIR}/entropy-{index:03}-errors.txt"
        )

        async def wait(proc, index):
            code = await proc.wait()
            return code, index

        tasks.append(wait(proc, index))
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
    proc = await asyncio.create_subprocess_shell(f"curl -X POST {PLAZA}/shutdown")
    await proc.wait()


async def main():
    # print("run peers")
    if await run_peers():
        exit(1)


if __name__ == "__main__":
    asyncio.run(main())
