cargo build --profile artifact --bin simple-entropy
rsync target/artifact/simple-entropy ubuntu@$(terraform -chdir=terraform output -raw service):
