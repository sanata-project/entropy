from subprocess import run, PIPE
from json import loads

print("#service ", end="", flush=True)
run("terraform -chdir=terraform output -raw service", shell=True, check=True)
print()
for host in loads(
    run(
        "terraform -chdir=terraform output -json hosts",
        shell=True,
        check=True,
        stdout=PIPE,
        text=True,
    ).stdout
):
    print(host)
