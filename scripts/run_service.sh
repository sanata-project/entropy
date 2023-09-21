SERVICE=$(python3 scripts/common.py SERVICE_HOST)
WORK_DIR=$(python3 scripts/common.py WORK_DIR)
cargo build --profile artifact --bin entropy
rsync target/artifact/entropy ${SERVICE}:${WORK_DIR}/entropy
ssh ${SERVICE} ${WORK_DIR}/entropy ${SERVICE} --plaza-service $(python3 scripts/common.py NUM_TOTAL_PEER)