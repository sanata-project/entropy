SERVICE=$(python3 scripts/common.py SERVICE_HOST)
WORK_DIR=$(python3 scripts/common.py WORK_DIR)
cargo build --profile artifact --bin entropy --color never
rsync target/artifact/entropy ${SERVICE}:${WORK_DIR}/entropy
ssh ${SERVICE} OTEL_SDK_DISABLED=true ${WORK_DIR}/entropy ${SERVICE} --plaza-service $(python3 scripts/common.py NUM_TOTAL_PEER)