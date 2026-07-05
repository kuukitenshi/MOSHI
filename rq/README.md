# Evaluation: Research Questions

Three RQs evaluate the partitioned broker's performance from different angles.
Each has its own subfolder with scripts, data outputs, and a README with all
command-line flags.

---

## Overview

| RQ | Question | Machines | Netem | Script |
|---|---|---|---|---|
| **RQ1** | Does the partitioned architecture add perceptible latency for the end user? | client + cosmos | lan (8ms +/- 2ms) | `rq1/run_e2e_cosmos.sh` |
| **RQ2** | What is the CPU cost of FROST vs Ed25519, and how does it scale with quorum size? | cosmos only | none | `rq2/run_rq2.sh` |
| **RQ3** | What is the system's peak throughput and saturation point under high load? | cosmos + ngstorage + vitamina01 | none (server-local) | `rq3/run.sh` |

---

## RQ1: End-to-End Latency

**What it measures:** wall-clock login time from client to authenticated session,
comparing the partitioned architecture (backend on `cosmos`) against a local app
using the live Hellō cloud broker. Both flows use Google OAuth with a
returning-user session.

**Run:**
```bash
./rq/rq1/run_e2e_cosmos.sh --iterations 50        # deploy + 50 runs (thesis)
./rq/rq1/run_e2e_cosmos.sh --no-build             # skip compilation
./rq/rq1/run_e2e_cosmos.sh --netem lan --no-build  # with network emulation
```

**Outputs:** `rq/rq1/out/`  |  `plots/rq1/`

Full flag reference: `rq/rq1/README.md`

---

## RQ2: Cryptographic Overhead

**What it measures:** pure CPU cost of FROST threshold signing vs centralised
Ed25519, across three quorum sizes (3,2), (5,3), (7,5). Runs as a standalone
process on `cosmos`: no network, no services.

**Run:**
```bash
./rq/rq2/run_rq2.sh             # build + deploy to cosmos + run + plot
./rq/rq2/run_rq2.sh --no-build  # skip compilation
```

Each configuration is measured over 20 independent executions (`--repeats`) of
2000 iterations, so the table can report the run-to-run `[min, max]` range next
to each median. After the run, `rq/rq2/out/rq2_table.tex` contains ready-to-paste
LaTeX table rows with the results; RQ2 has no figure in the thesis.

**Outputs:** `rq/rq2/out/`  |  `plots/rq2/`

Full flag reference: `rq/rq2/README.md`

---

## RQ3: System Throughput

**What it measures:** peak sustainable throughput and the saturation knee. Two
client machines drive **vegeta** at a rising-then-falling offered request rate
against the `POST /login` endpoint, while server CPU is sampled in parallel to
prove the ceiling is the server's, not the load generators'. The thesis uses only
the rising leg (`rq3_saturation_vegeta`); the down-leg feeds the optional
recovery figure.

**Run:**
```bash
# Full experiment + both figures (cluster hosts + sweep range baked in)
./rq/rq3/run.sh

# Push the sweep further / change cadence
./rq/rq3/run.sh --rate-end 9000 --rate-step 500 --duration 12
```

**Outputs:** `rq/rq3/out/`  |  `plots/rq3/`

Full flag reference: `rq/rq3/README.md`

---

## Regenerate plots from existing data

```bash
./generate_plots.sh
```

This reads from `rq/*/out/` and writes to `plots/*/`. It does not SSH anywhere
or re-run experiments: only re-renders the plots.
