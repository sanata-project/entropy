curl $(python3 scripts/common.py SERVICE)/shutdown -X POST
sleep 2
ssh $(python3 scripts/common.py SERVICE_HOST) pkill -TERM entropy || true