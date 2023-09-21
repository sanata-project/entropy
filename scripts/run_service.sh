SERVICE=$(terraform -chdir=terraform output -raw service)
cargo build --profile artifact --bin simple-entropy
rsync target/artifact/simple-entropy ubuntu@${SERVICE}:entropy
ssh ubuntu@${SERVICE} OTEL_SDK_DISABLED=true ./entropy ${SERVICE} --plaza-service $1
