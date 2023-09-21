SERVICE=$(python3 scripts/common.py service-host)
cargo build --profile artifact --bin simple-entropy
rsync target/artifact/simple-entropy ${SERVICE}:entropy
ssh ${SERVICE} ./entropy ${SERVICE} --plaza-service $(python3 scripts/common.py num-peer)