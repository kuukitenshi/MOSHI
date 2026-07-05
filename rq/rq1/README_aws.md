# RQ1: fair comparison on AWS (`deploy_aws.sh`)

To compare the prototype against Hellō **fairly**, the prototype backend must be reached
over the same kind of network path as the Hellō wallet. Hellō is hosted in AWS
**`us-west-2` (Oregon)**, so we deploy the prototype backend in that **same region** and
run the client from the laptop **without the Lisbon VPN**. Both backends are then
~90 ms from the client → a like-for-like end-to-end comparison.

> Topology: **client (browser + measurement) stays on your laptop**; only the
> **backend** (AB/IB/tTS/mock_idp) runs on the EC2, reached over an SSH tunnel: exactly
> like the cosmos setup, just a different `--server`.

---

## One-time setup

1. **AWS account** with billing enabled.
2. **Create an access key**: AWS Console → IAM → your user → *Security credentials* →
   *Create access key* (CLI). **Never paste the secret anywhere public.**
3. **Configure the CLI**:
   ```bash
   aws configure          # paste key id + secret, region = us-west-2, output = json
   aws sts get-caller-identity   # should print your account id
   ```
4. **Permissions**: IAM → your user → *Add permissions* → *Attach policies directly* →
   **`AmazonEC2FullAccess`** (covers launch / security group / key pair / describe-images).

---

## Commands

```bash
# 0. Turn the Lisbon VPN OFF (we want the direct Japan→Oregon path).

# 1. UP: launch a t3.medium in us-west-2 and wire the SSH alias `awsbench`
./rq/rq1/deploy_aws.sh up

# 2. RUN: deploy the stack + measure the demo on AWS (with LAN netem, as in the thesis)
./rq/rq1/deploy_aws.sh run --netem lan
#    (use --no-netem for raw localhost; any run_e2e_cosmos.sh flag is forwarded)

# 3. Measure the Hellō baseline from the laptop (also VPN off), as usual:
./rq/rq1/run_e2e_cosmos.sh --only hello

# 4. DOWN: terminate the instance (DO THIS when finished, to stop billing)
./rq/rq1/deploy_aws.sh down

# status: show the saved instance + its state
./rq/rq1/deploy_aws.sh status
```

### What `up` does automatically
- resolves the latest Amazon Linux 2023 AMI (via `describe-images`, no SSM needed);
- creates a key pair (`~/.ssh/moshi-bench-key.pem`) and a security group that opens
  **only SSH (22) from your current public IP**;
- launches the instance, waits for SSH, and writes the `awsbench` alias into `~/.ssh/config`.

### Results (generated locally: nothing is copied back)
- **Data:** `rq/rq1/out/` → `rq1_demo_summary.json`, `breakdown_demo.csv`,
  `har_demo.har`, `rq1_rtt.txt` (RTT should be ~90 ms now).
- **Plots:** `plots/rq1/` → `rq1_comparison`, `rq1_breakdown_moshi`,
  `rq1_browser_detail`, `rq1_breakdown_hello_har` (`.png` + `.pdf`).

---

## Notes & gotchas
- **Cost:** a `t3.medium` is ≈ \$0.04/h: a few cents per benchmark. **Always run
  `down` when done.** `AWS_INSTANCE_TYPE=t3.large ./deploy_aws.sh up` to override the size.
- The services bind to `localhost` on the EC2 and are reached only via the SSH tunnel;
  nothing but SSH is exposed.
- **netem** auto-downloads `toxiproxy` on the instance (needs outbound internet: it has it).
- Pin a specific AMI with `AWS_AMI=ami-xxxxxxxx ./deploy_aws.sh up` if the lookup fails.
- The cosmos cluster (Lisbon) is still used for **RQ2** and **RQ3** (those need the VPN).
