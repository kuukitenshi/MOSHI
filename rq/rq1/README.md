# RQ1: End-to-End Latency

Measures the total user-perceived authentication time comparing:
- **Partitioned architecture** (this work): backend on `cosmos`, client local
- **Hellō-based app** (production baseline): local Hellō quickstart app → live Hellō cloud broker

Both flows use Google OAuth with a returning-user session (pre-authenticated).

---

## Scripts

| Script | Purpose | Status |
|---|---|---|
| `run_e2e_cosmos.sh` | **Main RQ1 runner**: deploy backend to cosmos, measure both systems | Use this |
| `run_rq1_local.sh` | Local-only runner (all services on localhost): dev/debug | Use for local testing |
| `plot_rq1.sh` | Regenerate all RQ1 plots from existing data (no measurement) | Use this |
| `breakdown.js` | Per-phase breakdown: also writes runs CSV + summary JSON | Use this |
| `deploy_server.sh` | Build musl binaries and deploy server stack to remote SSH host | Use this |
| `deploy_aws.sh` | Launch/deploy/terminate an AWS EC2 backend for a like-for-like RQ1 (see `README_aws.md`) | Use this |
| `setup_sessions.js` | One-time browser login to save Google/Hellō session | Use this |
| `analyze_har.js` | Parse a captured HAR into per-host timings (used by `run_e2e_cosmos.sh`) | Use this |
| `plot_breakdown_cosmos.py` | Breakdown plot (4 panels, independent scales) | Use this |
| `plot_cdf.py` | Empirical CDF of E2E latency (moshi vs Hellō): the figure used in the thesis | Use this |
| `plot_comparison.py` | Aggregate stats bar chart (Mean/Median/P95): superseded by the CDF | Optional |
| `plot_hello_breakdown_har.py` | Hellō wallet breakdown from HAR (RTT-subtracted) | Use this |
| `plot_browser_detail.py` | Client-side per-host browser breakdown | Use this |

---

## How to run

### Full RQ1 measurement (cosmos backend, recommended)

```bash
# Both systems: demo on cosmos + hellō locally (50 runs each, as in the thesis)
bash rq/rq1/run_e2e_cosmos.sh --iterations 50

# Demo on cosmos only
bash rq/rq1/run_e2e_cosmos.sh --only demo --iterations 50

# Hellō locally only (no cosmos needed)
bash rq/rq1/run_e2e_cosmos.sh --only hello --iterations 50

# Skip deploy (server already running on cosmos)
bash rq/rq1/run_e2e_cosmos.sh --no-deploy --iterations 50

# Skip cargo build
bash rq/rq1/run_e2e_cosmos.sh --no-build --iterations 50

# Reset saved Google/Hellō session (re-runs interactive login)
bash rq/rq1/run_e2e_cosmos.sh --reset-sessions

# With network emulation on the server (inter-service links)
bash rq/rq1/run_e2e_cosmos.sh --netem lan       # 8ms one-way (default reference line)
bash rq/rq1/run_e2e_cosmos.sh --netem wifi      # 35ms one-way
bash rq/rq1/run_e2e_cosmos.sh --netem mobile    # 90ms one-way
bash rq/rq1/run_e2e_cosmos.sh --netem custom 120 25 0.5

# Different server
bash rq/rq1/run_e2e_cosmos.sh --server angainor
```

### Regenerate plots from existing data

```bash
bash rq/rq1/plot_rq1.sh
```

### Deploy server to cosmos manually

```bash
bash rq/rq1/deploy_server.sh --server cosmos
bash rq/rq1/deploy_server.sh --server cosmos --no-build
```

---

## Outputs

| File | Contents |
|---|---|
| `out/breakdown_demo.csv` | Per-run phase breakdown (A–H) for demo: one row per run |
| `out/rq1_demo_runs.csv` | Per-run elapsed_ms for demo (derived from breakdown) |
| `out/rq1_demo_summary.json` | Stats: mean, median, p95, stddev, min, max |
| `out/breakdown_hello.csv` | Per-run phase breakdown (A–D) for Hellō: one row per run |
| `out/rq1_hello_runs.csv` | Per-run elapsed_ms for Hellō (derived from breakdown) |
| `out/rq1_hello_summary.json` | Stats: mean, median, p95, stddev, min, max |
| `plots/rq1/rq1_breakdown_moshi.pdf` | 4-panel breakdown figure (independent scales) |
| `plots/rq1/rq1_cdf.pdf` | CDF of E2E login latency (moshi vs Hellō): the thesis figure |
| `plots/rq1/rq1_comparison.pdf` | Aggregate stats bar chart (Mean/Median/P95) |
