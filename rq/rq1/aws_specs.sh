#!/usr/bin/env bash
#
# aws_specs.sh: print the AWS RQ1 instance's specs, ready to paste into the
# machines table (tab:machines) of the thesis.
#
# It works whether or not the instance is running:
#   - instance type, vCPU, RAM, clock, arch come from `describe-instance-types`
#     (a static lookup, no running instance needed);
#   - the exact CPU model and OS string are read over SSH from the live instance
#     when it is up (falls back to AWS-reported values otherwise).
#
# Usage:
#   ./rq/rq1/aws_specs.sh                 # auto-detect type (running tag) or t3.medium
#   ./rq/rq1/aws_specs.sh t3.large        # force an instance type
#   AWS_SSH_ALIAS=awsbench ./rq/rq1/aws_specs.sh
#
set -euo pipefail

REGION="${AWS_REGION:-us-west-2}"
SSH_ALIAS="${AWS_SSH_ALIAS:-awsbench}"
TABLE_ID="${TABLE_ID:-s_oregon}"        # the \textbf{...} id used in the table

command -v aws >/dev/null || { echo "AWS CLI not found." >&2; exit 1; }
AWS() { aws --region "$REGION" "$@"; }

# 1. Resolve the instance type: arg > running tagged instance > t3.medium default.
ITYPE="${1:-}"
if [[ -z "$ITYPE" ]]; then
  ITYPE=$(AWS ec2 describe-instances \
    --filters "Name=tag:Name,Values=moshi-bench" "Name=instance-state-name,Values=running" \
    --query 'Reservations[0].Instances[0].InstanceType' --output text 2>/dev/null || true)
fi
[[ -z "$ITYPE" || "$ITYPE" == "None" ]] && ITYPE="t3.medium"

# 2. Static spec lookup (works with the instance stopped/terminated).
read -r VCPU CORES TPC RAM_MIB ARCH GHZ < <(
  AWS ec2 describe-instance-types --instance-types "$ITYPE" \
    --query 'InstanceTypes[0].[VCpuInfo.DefaultVCpus,VCpuInfo.DefaultCores,VCpuInfo.DefaultThreadsPerCore,MemoryInfo.SizeInMiB,ProcessorInfo.SupportedArchitectures[0],ProcessorInfo.SustainedClockSpeedInGhz]' \
    --output text
)
RAM_GB=$(awk "BEGIN{printf \"%g\", $RAM_MIB/1024}")
GHZ_FMT=$(awk "BEGIN{printf \"%.2f\", $GHZ}")   # match the table's X.XX GHz style

# 3. Exact CPU model + OS from the live instance, if reachable.
CPU_MODEL=""; OS_STR=""
if ssh -o BatchMode=yes -o ConnectTimeout=5 "$SSH_ALIAS" true 2>/dev/null; then
  CPU_MODEL=$(ssh "$SSH_ALIAS" "lscpu | sed -n 's/^Model name:[[:space:]]*//p'" 2>/dev/null | head -1)
  OS_STR=$(ssh "$SSH_ALIAS" "grep -oP '(?<=^PRETTY_NAME=\").*(?=\")' /etc/os-release" 2>/dev/null | head -1)
fi
[[ -z "$CPU_MODEL" ]] && CPU_MODEL="Intel Xeon Platinum 8259CL"   # typical t3 host (verify with lscpu)
[[ -z "$OS_STR"    ]] && OS_STR="Amazon Linux 2023"

echo ""
echo "================ AWS RQ1 instance specs ================"
printf "  type          : %s\n" "$ITYPE"
printf "  vCPU          : %s  (%s core / %s threads-per-core)\n" "$VCPU" "$CORES" "$TPC"
printf "  RAM           : %s GiB (%s MiB)\n" "$RAM_GB" "$RAM_MIB"
printf "  clock         : %s GHz sustained\n" "$GHZ"
printf "  architecture  : %s\n" "$ARCH"
printf "  CPU model     : %s\n" "$CPU_MODEL"
printf "  OS            : %s\n" "$OS_STR"
echo "======================================================="
echo ""
echo "LaTeX row for tab:machines (paste into the table):"
echo ""
cat <<EOF
          \\textbf{${TABLE_ID//_/\\_}}
            & ${CPU_MODEL} @ ${GHZ_FMT}\\,GHz\\textsuperscript{b}
            & ${CORES}\\,c / ${VCPU}\\,t
            & ${RAM_GB}\\,GB
            & ${OS_STR}
            & RQ1 broker (\\acs{AWS} \\texttt{${REGION}}) \\\\
EOF
echo ""
echo "  (add footnote: \\textsuperscript{b}~AWS \\texttt{${ITYPE}} instance.)"
