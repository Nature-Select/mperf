//! Repeat cold-start measurement N times to validate the kdebug
//! supervisor under iteration. Used during development to avoid the
//! cycle of "make change → ask user to click Start 5 times → paste log".
//!
//! Run:  cargo run --example startup_loop -p mperf-ios -- \
//!         <udid> <bundle_id> [iterations]
//!
//! Defaults: udid auto-discovered (errors if multiple iOS devices),
//! bundle_id = com.tyrell.eve, iterations = 5.

use anyhow::{Context, Result};
use std::time::Duration;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info,mperf_ios=debug")),
        )
        .with_target(true)
        .init();

    let args: Vec<String> = std::env::args().collect();
    let udid = match args.get(1).cloned() {
        Some(s) if !s.is_empty() => s,
        _ => discover_udid().await?,
    };
    let bundle_id = args
        .get(2)
        .cloned()
        .unwrap_or_else(|| "com.tyrell.eve".to_string());
    let iterations: usize = args
        .get(3)
        .and_then(|s| s.parse().ok())
        .unwrap_or(5);

    tracing::info!(udid, bundle_id, iterations, "startup_loop begin");

    let mut results: Vec<Option<u64>> = Vec::with_capacity(iterations);
    for i in 1..=iterations {
        tracing::info!(iter = i, "---- iteration starting ----");
        let t0 = std::time::Instant::now();
        match mperf_ios::measure_cold_start(&udid, &bundle_id).await {
            Ok(t) => {
                tracing::info!(
                    iter = i,
                    total_ms = t.total_ms,
                    elapsed_ms = t0.elapsed().as_millis() as u64,
                    "iteration succeeded"
                );
                results.push(Some(t.total_ms));
            }
            Err(e) => {
                tracing::error!(
                    iter = i,
                    error = %e,
                    elapsed_ms = t0.elapsed().as_millis() as u64,
                    "iteration FAILED"
                );
                results.push(None);
            }
        }
        // Inter-iteration wait — long enough that any iOS 26 kperf
        // release latency should have run to completion. Pass --
        // INTER_WAIT_SEC env to override.
        let inter_wait_sec: u64 = std::env::var("INTER_WAIT_SEC")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(3);
        if i < iterations {
            tracing::info!(seconds = inter_wait_sec, "sleeping between iterations");
            tokio::time::sleep(Duration::from_secs(inter_wait_sec)).await;
        }
    }

    println!();
    println!("===== summary =====");
    for (i, r) in results.iter().enumerate() {
        match r {
            Some(ms) => println!("  iter {}: {} ms", i + 1, ms),
            None => println!("  iter {}: FAIL", i + 1),
        }
    }
    let ok = results.iter().filter(|r| r.is_some()).count();
    println!("  {}/{} succeeded", ok, iterations);
    Ok(())
}

/// Pick a single iOS UDID from the device list. Errors if 0 or >1.
async fn discover_udid() -> Result<String> {
    let devices = mperf_ios::list_devices()
        .await
        .context("list_devices")?;
    let usable: Vec<_> = devices
        .into_iter()
        .filter(|d| d.usable)
        .collect();
    if usable.is_empty() {
        anyhow::bail!("no usable iOS devices found");
    }
    if usable.len() > 1 {
        let ids: Vec<String> = usable.iter().map(|d| d.id.clone()).collect();
        anyhow::bail!(
            "multiple iOS devices found, pass UDID explicitly: {:?}",
            ids
        );
    }
    Ok(usable[0].id.clone())
}
