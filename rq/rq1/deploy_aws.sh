#!/usr/bin/env bash
#
# deploy_aws.sh: provision an EC2 in us-west-2 (the same AWS region as the Hellō
# wallet) so the prototype can be benchmarked over the SAME client->server path as
# the baseline, removing the VPN/cross-continent asymmetry of the cosmos cluster.
#
# It only PROVISIONS the box and wires an SSH alias; the actual benchmark is then
# the existing run_e2e_cosmos.sh pointed at that alias (the deploy logic, tunnels
# and measurement are unchanged: only the --server target moves to AWS).
#
# Commands:
#   ./deploy_aws.sh up          launch the instance + write an `awsbench` SSH alias
#   ./deploy_aws.sh run         up (if needed) + run the demo benchmark on AWS, no VPN
#   ./deploy_aws.sh down        terminate the instance and clean up
#   ./deploy_aws.sh status      show the saved instance + its state
#
# Requirements (one-time, see the step-by-step in the chat):
#   - AWS CLI v2 installed and configured (`aws configure`, with us-west-2 access)
#   - permission to create key pairs, security groups and t3 instances
#
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
STATE="$ROOT/rq/rq1/.aws_bench_state"     # stores instance-id / sg-id / key path
SSH_ALIAS="awsbench"
SSH_CONFIG="$HOME/.ssh/config"

REGION="us-west-2"
INSTANCE_TYPE="${AWS_INSTANCE_TYPE:-t3.medium}"
KEY_NAME="moshi-bench-key"
KEY_FILE="$HOME/.ssh/${KEY_NAME}.pem"
SG_NAME="moshi-bench-sg"
SSH_USER="ec2-user"                       # Amazon Linux 2023 default user

GREEN='\033[0;32m'; CYAN='\033[0;36m'; YELLOW='\033[1;33m'; RED='\033[0;31m'; NC='\033[0m'
say()  { echo -e "${CYAN}[aws]${NC} $*"; }
ok()   { echo -e "${GREEN}[aws]${NC} $*"; }
warn() { echo -e "${YELLOW}[aws]${NC} $*"; }
die()  { echo -e "${RED}[aws] $*${NC}" >&2; exit 1; }

command -v aws >/dev/null || die "AWS CLI not found. Install it and run 'aws configure' first."

AWS() { aws --region "$REGION" "$@"; }

# ── provisioning ────────────────────────────────────────────────────────────
launch() {
  if [[ -f "$STATE" ]]; then
    # already have an instance? reuse if still alive
    # shellcheck disable=SC1090
    source "$STATE"
    local st
    st=$(AWS ec2 describe-instances --instance-ids "$INSTANCE_ID" \
          --query 'Reservations[0].Instances[0].State.Name' --output text 2>/dev/null || echo gone)
    if [[ "$st" == "running" || "$st" == "pending" ]]; then
      ok "Reusing existing instance $INSTANCE_ID ($st)."; return 0
    fi
    warn "Saved instance is '$st': provisioning a new one."
    rm -f "$STATE"
  fi

  # Resolve the latest Amazon Linux 2023 AMI. Prefer the SSM public parameter, but
  # fall back to ec2:DescribeImages (covered by AmazonEC2FullAccess) so SSM access
  # is not required. You can also pin one with AWS_AMI=ami-xxxxxxxx.
  local AMI="${AWS_AMI:-}"
  if [[ -z "$AMI" ]]; then
    say "Resolving latest Amazon Linux 2023 AMI in $REGION..."
    AMI=$(AWS ssm get-parameters \
          --names /aws/service/ami-amazon-linux-latest/al2023-ami-kernel-default-x86_64 \
          --query 'Parameters[0].Value' --output text 2>/dev/null || true)
  fi
  if [[ -z "$AMI" || "$AMI" == "None" ]]; then
    warn "SSM lookup unavailable; falling back to ec2 describe-images..."
    AMI=$(AWS ec2 describe-images --owners amazon \
          --filters "Name=name,Values=al2023-ami-2023.*-x86_64" "Name=state,Values=available" \
          --query 'sort_by(Images,&CreationDate)[-1].ImageId' --output text 2>/dev/null || true)
  fi
  [[ -n "$AMI" && "$AMI" != "None" ]] \
    || die "Could not resolve an AMI. Grant ec2:DescribeImages, or pin one with AWS_AMI=ami-xxxx."
  ok "AMI: $AMI"

  # key pair
  if [[ ! -f "$KEY_FILE" ]]; then
    say "Creating key pair $KEY_NAME..."
    AWS ec2 create-key-pair --key-name "$KEY_NAME" \
      --query 'KeyMaterial' --output text > "$KEY_FILE" 2>/dev/null \
      || die "Key pair creation failed (does $KEY_NAME already exist remotely but not locally? delete it in the console)."
    chmod 400 "$KEY_FILE"
    ok "Saved private key to $KEY_FILE"
  else
    say "Reusing local key $KEY_FILE"
  fi

  # security group (idempotent): allow SSH only from this machine's public IP
  local SG_ID
  SG_ID=$(AWS ec2 describe-security-groups --group-names "$SG_NAME" \
          --query 'SecurityGroups[0].GroupId' --output text 2>/dev/null || true)
  if [[ -z "$SG_ID" || "$SG_ID" == "None" ]]; then
    say "Creating security group $SG_NAME..."
    SG_ID=$(AWS ec2 create-security-group --group-name "$SG_NAME" \
            --description "moshi benchmark SSH access" --query 'GroupId' --output text)
  fi
  local MYIP
  MYIP=$(curl -fsS https://checkip.amazonaws.com | tr -d '\n')
  say "Authorising SSH (22) from your IP ${MYIP}/32..."
  AWS ec2 authorize-security-group-ingress --group-id "$SG_ID" \
    --protocol tcp --port 22 --cidr "${MYIP}/32" 2>/dev/null \
    || warn "Ingress rule already present (ok)."

  say "Launching $INSTANCE_TYPE..."
  local IID
  IID=$(AWS ec2 run-instances --image-id "$AMI" --instance-type "$INSTANCE_TYPE" \
        --key-name "$KEY_NAME" --security-group-ids "$SG_ID" \
        --tag-specifications 'ResourceType=instance,Tags=[{Key=Name,Value=moshi-bench}]' \
        --query 'Instances[0].InstanceId' --output text) || die "run-instances failed."
  ok "Instance $IID launching; waiting until running..."
  AWS ec2 wait instance-running --instance-ids "$IID"

  local IP
  IP=$(AWS ec2 describe-instances --instance-ids "$IID" \
        --query 'Reservations[0].Instances[0].PublicIpAddress' --output text)
  ok "Public IP: $IP"

  cat > "$STATE" <<EOF
INSTANCE_ID=$IID
SG_ID=$SG_ID
PUBLIC_IP=$IP
EOF

  write_ssh_alias "$IP"

  say "Waiting for SSH to come up..."
  for _ in $(seq 1 30); do
    if ssh -o BatchMode=yes -o ConnectTimeout=5 "$SSH_ALIAS" true 2>/dev/null; then
      ok "SSH ready."; break
    fi
    sleep 5
  done

  # iproute2 (tc) is needed only if you later use netem; AL2023 ships it. Confirm:
  ssh "$SSH_ALIAS" 'command -v tc >/dev/null || sudo dnf -y install iproute-tc >/dev/null 2>&1 || true' 2>/dev/null || true

  ok "Instance ready as SSH alias '${SSH_ALIAS}' (us-west-2)."
}

write_ssh_alias() {  # $1 = public ip
  local ip="$1"
  touch "$SSH_CONFIG"; chmod 600 "$SSH_CONFIG"
  # remove any previous block for this alias, then append a fresh one
  local tmp; tmp=$(mktemp)
  awk -v a="Host $SSH_ALIAS" '
    $0==a {skip=1; next}
    skip && /^Host / {skip=0}
    !skip {print}
  ' "$SSH_CONFIG" > "$tmp" && mv "$tmp" "$SSH_CONFIG"
  cat >> "$SSH_CONFIG" <<EOF

Host $SSH_ALIAS
    HostName $ip
    User $SSH_USER
    IdentityFile $KEY_FILE
    StrictHostKeyChecking accept-new
    ServerAliveInterval 30
EOF
  ok "Wrote SSH alias '$SSH_ALIAS' -> $ip into $SSH_CONFIG"
}

status() {
  [[ -f "$STATE" ]] || { warn "No saved instance."; return 0; }
  # shellcheck disable=SC1090
  source "$STATE"
  local st
  st=$(AWS ec2 describe-instances --instance-ids "$INSTANCE_ID" \
        --query 'Reservations[0].Instances[0].State.Name' --output text 2>/dev/null || echo gone)
  echo "  instance: $INSTANCE_ID   state: $st   ip: ${PUBLIC_IP:-?}   alias: $SSH_ALIAS"
}

terminate() {
  [[ -f "$STATE" ]] || { warn "No saved instance to terminate."; return 0; }
  # shellcheck disable=SC1090
  source "$STATE"
  say "Terminating $INSTANCE_ID..."
  AWS ec2 terminate-instances --instance-ids "$INSTANCE_ID" >/dev/null || true
  AWS ec2 wait instance-terminated --instance-ids "$INSTANCE_ID" 2>/dev/null || true
  rm -f "$STATE"
  ok "Terminated. (Security group $SG_NAME and key $KEY_NAME are kept for reuse; "
  echo "   delete them in the console if you want a full cleanup.)"
}

run_bench() {
  [[ "${1:-}" == "--" ]] && shift   # tolerate a separator before extra flags
  launch
  echo ""
  say "Running the demo benchmark on AWS (us-west-2), NO VPN, same region as Hellō..."
  warn "Make sure your Lisbon VPN is OFF for a clean direct path."
  # default profile is `lan`; pass --no-netem to start with raw localhost instead.
  "$ROOT/rq/rq1/run_e2e_cosmos.sh" --server "$SSH_ALIAS" --only demo "$@"
}

case "${1:-}" in
  up)        launch ;;
  run)       shift; run_bench "$@" ;;
  down|rm)   terminate ;;
  status)    status ;;
  *) echo "Usage: $0 {up|run|down|status}"; exit 1 ;;
esac
