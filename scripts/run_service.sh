SERVICE=$(python3 scripts/common.py service-host)
WORK_DIR=$(python3 scripts/common.py work-dir)
cargo build --profile artifact --bin simple-entropy
rsync target/artifact/simple-entropy ${SERVICE}:${WORK_DIR}/entropy
ssh ${SERVICE} ${WORK_DIR}/entropy ${SERVICE} --plaza-service $(python3 scripts/common.py num-peer)