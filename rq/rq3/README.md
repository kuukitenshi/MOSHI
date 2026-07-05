# RQ3: System Throughput and Concurrency

Stress-tests the stateless broker under high concurrent load to find the
**saturation knee** and the maximum sustainable throughput, and to prove the
ceiling is the *server's*, not the load generators'.

**Topology:**
- **Server** (`cosmos`): AB + IB + tTS + Mock IdP
- **Client1** (`ngstorage`), **Client2** (`vitamina01`): concurrent `POST /login`

Load is generated with **vegeta** over persistent (`keep-alive`) connections.
Both clients ramp the **offered request rate** UP past saturation and then back
DOWN, in lock-step (so the combined rate at the server is twice the per-client
rate). The down-leg is what shows the server *recovering* after overload.

The server stack is started with `RUST_LOG=warn` and a raised file-descriptor
limit (`ulimit -n 65535`). This matters: with the default `RUST_LOG=info` the
broker logs ~10 lines per request, and that synchronous logging: not file
descriptors: throttles the threads and makes the server collapse instead of
saturating gracefully.

A second process (`server_cpu_sampler.sh`) records whole-machine **server CPU%**
once per second during the run; each sweep level is aligned to it by timestamp,
so the figures can show server CPU rising to its own ceiling while the clients
stay idle: the direct proof of where the bottleneck is.

---

## Two figures

The sweep ramps the offered rate up past saturation and back down, then renders
**two views** of the same run:

### 1. `plots/rq3/rq3_saturation_vegeta`: saturation curve (`plot_saturation.py`)

The classic load curve: only the **rising leg** is plotted, **x-axis = combined
offered rate (req/s)**. Four panels:

1. **Latency** (P85 data point + mean/P95/P99, log): flat until the knee, then
   climbs.
2. **Throughput**: achieved vs the ideal `achieved = offered` diagonal + the
   ceiling line. Where achieved peels away from the diagonal = saturation.
3. **Errors %**.
4. **CPU** (log): both client generators **and** server CPU. The clients stay
   far below the 85% guardrail while server CPU rises toward its ceiling: proof
   the bottleneck is the server.

Vertical markers: **max sustainable** rate (highest offered rate still at ~0%
errors) and the **saturation knee** (elbow of the achieved-throughput curve).

### 2. `plots/rq3/rq3_saturation_recovery`: saturation **+** recovery (`plot_combined.py`)

The **whole ramp** (up *then* back down), **x-axis = ramp sequence (time →)**,
ticks labelled by the offered rate. Same four panels but **Throughput first**,
showing the load fall back and the system recover:

1. **Throughput**: offered (filled) vs achieved + ceiling. A red `peak / release`
   line and green-shaded **recovery** region mark the down-leg.
2. **Latency** (P85 + mean/P95/P99, log): climbs under overload, falls on the
   down-leg.
3. **Errors / dropped %**: rise under overload, **fall back to 0 on recovery**.
4. **CPU** (log): client1, client2, and server CPU vs the 85% guardrail.

> The peak rate is capped (default 1000→7000 req/s per client, 14000 combined)
> on purpose. Pushing far past the knee forces each client to juggle a huge
> backlog of open connections (latency climbs into seconds), and *then* the
> client CPU rises: those extreme points are contaminated, so they are not
> measured.

---

## Scripts

| Script | Purpose |
|---|---|
| `run.sh` | **Ready-to-run wrapper**: cluster hosts + sweep range baked in. |
| `run_rq3.sh` | Orchestrator (start stack, run sweep, plot). |
| `run_vegeta_sweep.sh` | Starts the server stack + CPU sampler and drives both clients. |
| `load_vegeta_client.sh` | Per-client rate generator (`vegeta -rate=R`, `--recovery` up-then-down ramp), runs on each client. |
| `server_cpu_sampler.sh` | Server-side CPU% sampler (`<epoch> <cpu%>` per second), runs on the server. |
| `plot_saturation.py` | → `rq3_saturation_vegeta` (saturation curve, rising leg vs offered rate) |
| `plot_combined.py` | → `rq3_saturation_recovery` (full up+down ramp vs time) |

---

## How to run

```bash
# Full experiment + both figures (cluster flags baked in)
./rq/rq3/run.sh

# Push the sweep further / change cadence (watch client CPU in panel 4!)
./rq/rq3/run.sh --rate-end 9000 --rate-step 500 --duration 12
```

> Needs `/tmp/vegeta` locally (it is scp'd to the clients and cached there).
> If missing:
> ```bash
> curl -fsSL -o /tmp/v.tgz \
>   https://github.com/tsenart/vegeta/releases/download/v12.12.0/vegeta_12.12.0_linux_amd64.tar.gz
> tar xzf /tmp/v.tgz -C /tmp vegeta && chmod +x /tmp/vegeta && rm /tmp/v.tgz
> ```

Regenerate both figures from existing data (no cluster, no SSH):

```bash
./generate_plots.sh        # all RQs
# or just RQ3:
./venv/bin/python rq/rq3/plot_saturation.py \
  --client1 rq/rq3/out/rq3_vegeta_client1.csv \
  --client2 rq/rq3/out/rq3_vegeta_client2.csv \
  --server-cpu rq/rq3/out/rq3_server_cpu.log \
  --output plots/rq3/rq3_saturation_vegeta
./venv/bin/python rq/rq3/plot_combined.py \
  --client1 rq/rq3/out/rq3_vegeta_client1.csv \
  --client2 rq/rq3/out/rq3_vegeta_client2.csv \
  --server-cpu rq/rq3/out/rq3_server_cpu.log \
  --output plots/rq3/rq3_saturation_recovery
```

---

## Data

| File | Contents |
|---|---|
| `out/rq3_vegeta_client{1,2}.csv` | Per-level stats per client: step, phase (up/down), offered rate, achieved RPS, latency percentiles, success %, client CPU %, level start epoch. |
| `out/rq3_server_cpu.log` | `<epoch> <cpu%>` per second on the server, aligned to each level by timestamp. |
