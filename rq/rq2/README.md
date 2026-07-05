# RQ2: Cryptographic Overhead and Quorum Scalability

Measures the pure CPU cost of replacing a centralised Ed25519 signature with the
FROST threshold signature scheme, and how that cost scales across quorum sizes.
All benchmarks run on **cosmos** (no network, no client involved).

Configurations evaluated:
- `Ed25519` (centralised baseline)
- `FROST (n=3, t=2)`: minimum fault-tolerant quorum
- `FROST (n=5, t=3)`
- `FROST (n=7, t=5)`

**Measurement design.** A single Ed25519 signature takes a few tens of
microseconds, i.e. it sits at the machine's timing-noise floor, so one execution
proves nothing. Each configuration is therefore measured over **20 independent
executions** (separate processes, each pinned to one core with `taskset`) of
**2000 iterations** after **500 warm-up** iterations, all driven from a fixed RNG
seed. Results are reported as the median across executions with the run-to-run
`[min, max]` range next to it: that range is the point: the Ed25519 baseline's
own median swings by ~1.85x while every FROST median holds within 7%, which is
why the ratio to the baseline is not a meaningful figure of merit and the
absolute cost is.

---

## Scripts

| Script | Purpose |
|---|---|
| `run_rq2.sh` | Build, deploy to cosmos, run the 20 executions, fetch results, generate the table |
| `plot_rq2.py` | Aggregate `out/runs/` + print/save the LaTeX table rows (`out/rq2_table.tex`) |
| `plot_quorum.py` | Quorum-scalability plot (absolute latency + overhead vs protocol messages) |
| `plot_rq2_merged.py` | Optional single-figure version of the same data: the thesis uses no RQ2 figure |

The benchmark binary lives in `bench_frost/` at the project root (Rust crate).

---

## How to run

```bash
# Full run: build musl binary, deploy to cosmos, run, fetch, plot
./rq/rq2/run_rq2.sh

# Skip cargo build (binary already compiled)
./rq/rq2/run_rq2.sh --no-build

# Different server
./rq/rq2/run_rq2.sh --server angainor

# More iterations per execution (default: 2000)
./rq/rq2/run_rq2.sh --iterations 4000

# More warmup iterations, discarded (default: 500)
./rq/rq2/run_rq2.sh --warmup 1000

# Fewer/more independent executions (default: 20): this is what sets the
# [min,max] run-to-run range in the table; 1 gives a degenerate range.
./rq/rq2/run_rq2.sh --repeats 5

# All flags
./rq/rq2/run_rq2.sh [--server HOST] [--iterations N] [--warmup N] [--repeats N] [--no-build]
```

---

## Outputs

| File | Contents |
|---|---|
| `rq/rq2/out/runs/results_N.json` | Raw stats of each of the 20 independent executions |
| `rq/rq2/out/bench_frost_results.json` | The executions aggregated by `plot_rq2.py` (median, p95, p99, per-execution medians) |
| `rq/rq2/out/rq2_table.tex` | Ready-to-paste LaTeX table rows: columns: Method, protocol msgs (`4t`), median `[min-max]`, P95, P99, `xN` vs Ed25519, `xn` vs `(3,2)` |
| `plots/rq2/rq2_crypto_overhead.pdf` | 2-panel plot (log-scale bars + distribution strips): not used in the thesis, the table replaces it |
| `plots/rq2/rq2_quorum_scalability.pdf` | Scaling plot (absolute latency + relative overhead vs protocol messages) |

---

## Regenerate plots from existing data

```bash
# Table + overhead plot (aggregates the per-execution data in out/runs/).
# Always pass --runs-dir: --input reads the single aggregated JSON, whose
# repeats=1 collapses the [min,max] range to a single point.
./venv/bin/python rq/rq2/plot_rq2.py \
  --runs-dir rq/rq2/out/runs --output plots/rq2/rq2_crypto_overhead

# Quorum scalability plot (reads bench_frost_results.json)
./venv/bin/python rq/rq2/plot_quorum.py \
  --input rq/rq2/out/bench_frost_results.json --output plots/rq2/rq2_quorum_scalability
```

Or simply `./generate_plots.sh` to re-render every RQ at once.

After running `./rq/rq2/run_rq2.sh`, check `rq/rq2/out/rq2_table.tex` for the
LaTeX table rows and paste them into the `\begin{tabular}` in the thesis.
