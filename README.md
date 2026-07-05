# MOSHI

## Configuration (required before first run)

Two local files hold secrets and are **not** included in this repository, so you
must create them yourself. Both are git-ignored; never commit them.

- **`.env`** (required to run the prototype). Copy the template and fill in real
  OAuth credentials, since the IB service will not start without all six values:

  ```bash
  cp .env.example .env
  # then edit .env with your Google / GitHub / Discord OAuth client IDs & secrets
  ```

  Create the credentials in each provider's console and set the redirect/callback
  URL to the matching IB endpoint (`http://localhost:4020/callback/{google,github,discord}`).

- **`.aws`** (only needed for the AWS deployment scripts). Create it with your own
  AWS access key / secret if you intend to run the cloud experiments; it is not
  required for the local prototype.

> **TLS certificates (`certs/`)** are **not** shipped and do not need to be
> created by hand: the tTS generates them automatically on first start (a
> self-signed root CA plus per-service certs, via `rcgen`). The `certs/` folder
> is git-ignored and will appear on the first run.

## Run the prototype: local

```bash
./run.sh
# open http://localhost:3000
```

```bash
./run.sh --netem lan      # with inter-service network emulation (8ms)
./run.sh --no-build       # skip cargo build
```

## Run the prototype: cluster (INESC VPN on)

```bash
# VPN must be connected (SSH key: ~/.ssh/id_inesc_cluster_lcunha)
./rq/rq1/deploy_server.sh --server cosmos             # build musl + deploy + start stack
./rq/rq1/deploy_server.sh --server cosmos --no-build  # redeploy without rebuilding
```

## Run each RQ

### RQ1
```bash
# RQ1: end-to-end latency
./rq/rq1/run_rq1_local.sh             # local (all on localhost)
./rq/rq1/run_e2e_cosmos.sh            # cluster: cosmos backend (VPN on)
./rq/rq1/run_e2e_cosmos.sh --no-build # --only x to only run one
./rq/rq1/run_e2e_cosmos.sh --netem lan --no-build 
```

```bash
# RQ1: fair AWS comparison (VPN OFF, backend in us-west-2)
./rq/rq1/deploy_aws.sh up                 # launch EC2 + wire SSH alias
./rq/rq1/deploy_aws.sh run --netem lan    # deploy stack + measure demo on AWS
./rq/rq1/run_e2e_cosmos.sh --only hello   # Hellō baseline from laptop (VPN off)
./rq/rq1/deploy_aws.sh down               # terminate (stop billing)
```

### RQ2
```bash
# RQ2: cryptographic overhead (cosmos, VPN on)
./rq/rq2/run_rq2.sh
./rq/rq2/run_rq2.sh --no-build
```

### RQ3
```bash
# RQ3: throughput / saturation (cosmos + ngstorage + vitamina01, VPN on)
./rq/rq3/run.sh
./rq/rq3/run.sh --rate-end 9000 --rate-step 500 --duration 12
```

## Regenerate plots (no experiments, no SSH)

```bash
./generate_plots.sh
```
