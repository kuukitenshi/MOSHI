//! bench_frost: RQ2: FROST threshold signing vs centralised Ed25519
//!
//! Runs on the server (cosmos). Outputs JSON so run_rq2.sh can fetch it.
//!
//! Usage:
//!   bench_frost [--iterations 1000] [--warmup 50] [--out results.json]

use frost_ed25519 as frost;
use rand_chacha::ChaCha20Rng;
use rand_core::SeedableRng;
use std::collections::BTreeMap;
use std::time::Instant;

/// Fixed seed so every run uses an identical cryptographic workload (keys,
/// nonces, messages), making the benchmark reproducible. Note: this fixes the
/// *workload*, not the measured wall-clock latency, which still varies with CPU
/// turbo, frequency scaling and OS scheduling. ChaCha20Rng is a CSPRNG, so the
/// FROST/Ed25519 operations remain cryptographically valid.
const RQ2_SEED: u64 = 0x5251_3253_5252_3242; // "RQ2RRB": arbitrary fixed value

// ── CLI helpers ───────────────────────────────────────────────────────────────

fn arg(flag: &str, default: &str) -> String {
    let args: Vec<String> = std::env::args().collect();
    if let Some(i) = args.iter().position(|a| a == flag) {
        args.get(i + 1).cloned().unwrap_or_else(|| default.to_owned())
    } else {
        default.to_owned()
    }
}

// ── Statistics ────────────────────────────────────────────────────────────────

fn percentile(data: &mut Vec<f64>, p: f64) -> f64 {
    data.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let idx = ((p / 100.0) * (data.len() - 1) as f64).round() as usize;
    data[idx]
}

fn mean(data: &[f64]) -> f64 {
    data.iter().sum::<f64>() / data.len() as f64
}

fn stddev(data: &[f64]) -> f64 {
    let m = mean(data);
    (data.iter().map(|x| (x - m).powi(2)).sum::<f64>() / data.len() as f64).sqrt()
}

fn summarise(label: &str, mut times: Vec<f64>) -> serde_json::Value {
    times.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n    = times.len();
    let mean = mean(&times);
    let std  = stddev(&times);
    let med  = percentile(&mut times.clone(), 50.0);
    let p95  = percentile(&mut times.clone(), 95.0);
    let p99  = percentile(&mut times.clone(), 99.0);
    let min  = times[0];
    let max  = times[n - 1];
    println!("  {label}");
    println!("    n={n}  mean={mean:.3}µs  median={med:.3}µs  std={std:.3}µs");
    println!("    P95={p95:.3}µs  P99={p99:.3}µs  min={min:.3}µs  max={max:.3}µs");
    println!();
    serde_json::json!({
        "label": label, "n": n,
        "mean_us": mean, "median_us": med, "std_us": std,
        "p95_us":  p95,  "p99_us":   p99,
        "min_us":  min,  "max_us":   max,
        "raw_us":  times
    })
}

// ── Ed25519 centralised baseline ──────────────────────────────────────────────

fn bench_ed25519(iterations: usize, warmup: usize) -> Vec<f64> {
    use ed25519_dalek::{Signer, SigningKey};
    let mut rng = ChaCha20Rng::seed_from_u64(RQ2_SEED);
    let key = SigningKey::generate(&mut rng);
    let msg = b"eyJhbGciOiJFZERTQSJ9.eyJzdWIiOiJ0ZXN0In0";
    for _ in 0..warmup { let _ = key.sign(msg); }
    (0..iterations).map(|_| {
        let t = Instant::now();
        let _ = key.sign(msg);
        t.elapsed().as_secs_f64() * 1_000_000.0
    }).collect()
}

// ── FROST threshold signing ───────────────────────────────────────────────────

fn bench_frost(iterations: usize, warmup: usize, max_signers: u16, min_signers: u16) -> Vec<f64> {
    let mut rng = ChaCha20Rng::seed_from_u64(RQ2_SEED);
    let (shares, pubkey_pkg) = frost::keys::generate_with_dealer(
        max_signers, min_signers,
        frost::keys::IdentifierList::Default, &mut rng,
    ).expect("DKG");

    let mut key_pkgs = BTreeMap::new();
    for (id, share) in &shares {
        key_pkgs.insert(*id, frost::keys::KeyPackage::try_from(share.clone()).unwrap());
    }
    let signer_ids: Vec<_> = key_pkgs.keys().take(min_signers as usize).cloned().collect();
    let msg = b"eyJhbGciOiJFZERTQSJ9.eyJzdWIiOiJ0ZXN0In0";

    let run_once = |rng: &mut ChaCha20Rng| {
        let t = Instant::now();
        let mut nonces = BTreeMap::new();
        let mut commits = BTreeMap::new();
        for id in &signer_ids {
            let (n, c) = frost::round1::commit(key_pkgs[id].signing_share(), rng);
            nonces.insert(*id, n); commits.insert(*id, c);
        }
        let pkg = frost::SigningPackage::new(commits, msg);
        let mut shares = BTreeMap::new();
        for id in &signer_ids {
            shares.insert(*id, frost::round2::sign(&pkg, &nonces[id], &key_pkgs[id]).unwrap());
        }
        let sig = frost::aggregate(&pkg, &shares, &pubkey_pkg).unwrap();
        pubkey_pkg.verifying_key().verify(msg, &sig).unwrap();
        t.elapsed().as_secs_f64() * 1_000_000.0
    };

    // warmup
    for _ in 0..warmup { run_once(&mut rng); }
    (0..iterations).map(|_| run_once(&mut rng)).collect()
}

// ── Main ──────────────────────────────────────────────────────────────────────

fn main() {
    let iterations: usize = arg("--iterations", "1000").parse().unwrap();
    let warmup:     usize = arg("--warmup",     "50"  ).parse().unwrap();
    let out_path           = arg("--out", "bench_frost_results.json");

    println!("╔══════════════════════════════════════════════════╗");
    println!("║  RQ2: FROST vs Ed25519: cryptographic overhead  ║");
    println!("║  iterations={iterations}  warmup={warmup}                     ║");
    println!("╚══════════════════════════════════════════════════╝");
    println!();

    // ── Ed25519 baseline ──────────────────────────────────────────────────────
    println!("[1/4] Ed25519 (centralized, warmup {warmup})…");
    let ed_times = bench_ed25519(iterations, warmup);
    let ed_stats = summarise("Ed25519 (centralized)", ed_times);

    // ── FROST configurations ──────────────────────────────────────────────────
    let configs: &[(u16, u16)] = &[(3, 2), (5, 3), (7, 5)];
    let mut frost_results = Vec::new();
    for (i, &(n, t)) in configs.iter().enumerate() {
        println!("[{}/4] FROST ({n},{t}) (warmup {warmup})…", i + 2);
        let times = bench_frost(iterations, warmup, n, t);
        let label = format!("FROST Ed25519 (n={n}, t={t})");
        frost_results.push(summarise(&label, times));
    }

    // ── Write JSON ────────────────────────────────────────────────────────────
    let result = serde_json::json!({
        "iterations": iterations,
        "warmup":     warmup,
        "ed25519":    ed_stats,
        "frost":      frost_results,
    });

    std::fs::write(&out_path, serde_json::to_string_pretty(&result).unwrap()).unwrap();
    println!("Results saved to: {out_path}");
}
