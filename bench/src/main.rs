//! Speed and memory against the spec's targets (Hard constraints, 6):
//! - peak memory at or under 300 MB with 20,000 items;
//! - a daily check with up to 2,000 new orders in under 60 seconds;
//! - the first sync of 12 months and up to 100,000 orders in under
//!   10 minutes on a normal office connection.
//!
//! The mock Square runs in its own process with a simulated connection
//! (latency per request plus a bandwidth cap), and each check runs in its own
//! process, so the memory measured is the check's alone.
//!
//!   dsf-perf run [--items 20000] [--orders 100000] [--latency-ms 300] [--mbps 20] [--out perf.json]

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde_json::{Value, json};

fn arg(args: &[String], name: &str, default: &str) -> String {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
        .unwrap_or_else(|| default.to_string())
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("serve") => serve(&args),
        Some("check") => check(&args),
        Some("run") => std::process::exit(run(&args)),
        _ => {
            eprintln!(
                "usage: dsf-perf run [--items N] [--orders N] [--latency-ms N] [--mbps N] [--out file]"
            );
            std::process::exit(2);
        }
    }
}

/// The mock Square in its own process. Prints one JSON line with what the
/// check needs, then takes commands on stdin.
fn serve(args: &[String]) {
    let items: usize = arg(args, "--items", "20000").parse().unwrap();
    let orders: usize = arg(args, "--orders", "100000").parse().unwrap();
    let latency: u64 = arg(args, "--latency-ms", "300").parse().unwrap();
    let mbps: f64 = arg(args, "--mbps", "20").parse().unwrap();
    let scenario =
        dsf_gen::scenario_sized(7, dsf_gen::Size { items, max_variations: 5, orders, locations: 3 });
    let now = scenario.first.at;
    let variation_ids: Vec<String> = scenario
        .mock
        .catalog
        .iter()
        .flat_map(|o| {
            o.pointer("/item_data/variations").and_then(Value::as_array).cloned().unwrap_or_default()
        })
        .filter_map(|v| v["id"].as_str().map(str::to_string))
        .collect();
    let location_ids: Vec<String> =
        scenario.mock.locations.iter().filter_map(|l| l["id"].as_str().map(str::to_string)).collect();
    let mock = dsf_mock::MockSquare::start(scenario.mock, dsf_compute::LABEL_KEY);
    let (access, refresh) = {
        let mut s = mock.state();
        s.clock = Some(now);
        s.faults.latency = Duration::from_millis(latency);
        s.faults.bytes_per_second = (mbps * 1_000_000.0 / 8.0) as u64;
        s.issue_tokens(&[
            "MERCHANT_PROFILE_READ",
            "ITEMS_READ",
            "ORDERS_READ",
            "INVENTORY_READ",
            "ITEMS_WRITE",
        ])
    };
    println!(
        "{}",
        json!({
            "addr": mock.addr().to_string(), "access": access, "refresh": refresh,
            "now": now.to_rfc3339(), "window": scenario.window_days, "locations": scenario.selected_locations,
            "variations": variation_ids.len(),
        })
    );
    std::io::stdout().flush().unwrap();
    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        let line = line.unwrap_or_default();
        let parts: Vec<&str> = line.split_whitespace().collect();
        match parts.as_slice() {
            ["add-orders", count, at] => {
                let count: usize = count.parse().unwrap_or(0);
                let at: DateTime<Utc> = at.parse().unwrap();
                let mut s = mock.state();
                s.clock = Some(at);
                let mut x: u64 = 0x2545_F491_4F6C_DD1D;
                let new: Vec<Value> = (0..count)
                    .map(|i| {
                        x ^= x << 13;
                        x ^= x >> 7;
                        x ^= x << 17;
                        let var = &variation_ids[(x as usize) % variation_ids.len()];
                        let loc = &location_ids[(x as usize / 7) % location_ids.len()];
                        let closed = at - chrono::Duration::minutes((count - i) as i64 % 1000);
                        let t = closed.to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
                        json!({ "id": format!("PERF-{i:06}"), "location_id": loc, "state": "COMPLETED",
                                "closed_at": t, "updated_at": t,
                                "line_items": [{ "uid": "1", "catalog_object_id": var, "quantity": "1", "item_type": "ITEM" }] })
                    })
                    .collect();
                s.add_orders(new);
                println!("ok");
            }
            ["stats"] => {
                let s = mock.state();
                let violations = s.write_violations.len();
                println!(
                    "{}",
                    json!({ "requests": s.log.len(), "writes": s.writes.len(), "violations": violations })
                );
            }
            ["quit"] => break,
            _ => println!("?"),
        }
        std::io::stdout().flush().unwrap();
    }
}

fn peak_rss_mb() -> f64 {
    #[cfg(target_os = "linux")]
    {
        let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
        for line in status.lines() {
            if let Some(kb) = line.strip_prefix("VmHWM:") {
                return kb.trim().trim_end_matches("kB").trim().parse::<f64>().unwrap_or(0.0) / 1024.0;
            }
        }
        0.0
    }
    #[cfg(not(target_os = "linux"))]
    {
        let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
        unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) };
        // Bytes on macOS.
        usage.ru_maxrss as f64 / (1024.0 * 1024.0)
    }
}

/// One check in its own process, against the mock.
fn check(args: &[String]) {
    let addr: std::net::SocketAddr = arg(args, "--addr", "").parse().unwrap();
    let now: DateTime<Utc> = arg(args, "--now", "").parse().unwrap();
    let dir = std::path::PathBuf::from(arg(args, "--data-dir", ""));
    let labels = arg(args, "--labels", "on") == "on";
    let window: u32 = arg(args, "--window", "90").parse().unwrap();
    let cache = dsf_sync::Cache::open(&dsf_check::paths::cache_file(&dir)).unwrap();
    let mut settings = cache.settings().unwrap();
    settings.setup_done = true;
    settings.labels_on = labels;
    settings.window_days = window;
    cache.save_settings(&settings).unwrap();
    drop(cache);
    let tokens_path = dir.join("perf-tokens.json");
    let store = dsf_square::MemoryStore::default();
    let saved: Option<dsf_square::Tokens> =
        std::fs::read(&tokens_path).ok().and_then(|b| serde_json::from_slice(&b).ok());
    let tokens = saved.unwrap_or_else(|| dsf_square::Tokens {
        access_token: arg(args, "--access", ""),
        access_expires_at: now + chrono::Duration::days(30),
        refresh_token: arg(args, "--refresh", ""),
        refresh_expires_at: None,
        obtained_at: now,
        merchant_id: String::new(),
        scopes: ["MERCHANT_PROFILE_READ", "ITEMS_READ", "ORDERS_READ", "INVENTORY_READ", "ITEMS_WRITE"]
            .iter()
            .map(|s| s.to_string())
            .collect(),
        environment: "mock".into(),
    });
    dsf_square::TokenStore::save(&store, &tokens).unwrap();
    let notifier = dsf_notify::Recorder::default();
    let clock = move || now;
    let cfg = dsf_check::Config {
        app_id: dsf_mock::MOCK_APP_ID.into(),
        environment: dsf_square::Environment::Mock(addr),
        data_dir: dir.clone(),
    };
    let deps = dsf_check::Deps {
        tokens: &store,
        notifier: &notifier,
        clock: &clock,
        retry: dsf_square::RetryPolicy::default(),
        network_wait: Duration::ZERO,
        observer: None,
        progress: None,
    };
    let started = Instant::now();
    let outcome = dsf_check::run(&cfg, &deps, dsf_schedule::Trigger::Manual);
    let elapsed = started.elapsed();
    if let Ok(Some(t)) = dsf_square::TokenStore::load(&store) {
        // Hand-off for the test only: a scratch folder that is deleted after.
        let _ = std::fs::write(&tokens_path, serde_json::to_vec(&t).unwrap());
    }
    println!(
        "{}",
        json!({ "seconds": elapsed.as_secs_f64(), "peak_rss_mb": peak_rss_mb(), "outcome": outcome })
    );
}

fn run(args: &[String]) -> i32 {
    let me = std::env::current_exe().unwrap();
    let items = arg(args, "--items", "20000");
    let orders = arg(args, "--orders", "100000");
    let latency = arg(args, "--latency-ms", "300");
    let mbps = arg(args, "--mbps", "20");
    let out = arg(args, "--out", "perf.json");
    let generated = Instant::now();
    let mut server = Command::new(&me)
        .args(["serve", "--items", &items, "--orders", &orders, "--latency-ms", &latency, "--mbps", &mbps])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("start the mock");
    let mut server_out = BufReader::new(server.stdout.take().unwrap());
    let mut server_in = server.stdin.take().unwrap();
    let mut line = String::new();
    server_out.read_line(&mut line).unwrap();
    let info: Value = serde_json::from_str(&line).expect("mock info");
    eprintln!("mock ready in {:.1} s: {info}", generated.elapsed().as_secs_f64());
    let dir = tempfile_dir();
    let now: DateTime<Utc> = info["now"].as_str().unwrap().parse().unwrap();
    let window = info["window"].to_string();
    let addr = info["addr"].as_str().unwrap().to_string();
    let access = info["access"].as_str().unwrap().to_string();
    let refresh = info["refresh"].as_str().unwrap().to_string();
    let run_check = |at: DateTime<Utc>, labels: &str| -> Value {
        let o = Command::new(&me)
            .args([
                "check",
                "--addr",
                &addr,
                "--access",
                &access,
                "--refresh",
                &refresh,
                "--now",
                &at.to_rfc3339(),
                "--data-dir",
                &dir.to_string_lossy(),
                "--labels",
                labels,
                "--window",
                &window,
            ])
            .output()
            .expect("run a check");
        serde_json::from_slice(&o.stdout)
            .unwrap_or_else(|_| json!({ "error": String::from_utf8_lossy(&o.stderr) }))
    };
    let mut ask = |cmd: &str| -> String {
        writeln!(server_in, "{cmd}").unwrap();
        server_in.flush().unwrap();
        let mut reply = String::new();
        server_out.read_line(&mut reply).unwrap();
        reply
    };
    eprintln!("first sync (labels off)…");
    let first = run_check(now, "off");
    eprintln!("{first}");
    eprintln!("first labels…");
    let labels = run_check(now + chrono::Duration::hours(1), "on");
    eprintln!("{labels}");
    let next_day = now + chrono::Duration::days(1);
    ask(&format!("add-orders 2000 {}", next_day.to_rfc3339()));
    eprintln!("daily check with 2,000 new orders…");
    let daily = run_check(next_day, "on");
    eprintln!("{daily}");
    let stats: Value = serde_json::from_str(&ask("stats")).unwrap_or(Value::Null);
    let _ = ask("quit");
    let _ = server.wait();
    let _ = std::fs::remove_dir_all(&dir);

    let secs = |v: &Value| v["seconds"].as_f64().unwrap_or(f64::MAX);
    let rss = |v: &Value| v["peak_rss_mb"].as_f64().unwrap_or(f64::MAX);
    let peak = rss(&first).max(rss(&labels)).max(rss(&daily));
    let ok_outcome = |v: &Value| v["outcome"]["outcome"] == "done";
    let checks = [
        ("first sync of 12 months under 10 minutes", secs(&first) < 600.0 && ok_outcome(&first)),
        ("daily check with 2,000 new orders under 60 seconds", secs(&daily) < 60.0 && ok_outcome(&daily)),
        ("peak memory at or under 300 MB", peak <= 300.0),
        ("every write changed only the label", stats["violations"] == 0),
    ];
    let report = json!({
        "items": items, "orders": orders, "variations": info["variations"],
        "connection": { "latency_ms": latency, "mbps": mbps },
        "first_sync": first, "first_labels": labels, "daily": daily, "peak_rss_mb": peak, "mock": stats,
        "targets": checks.iter().map(|(n, ok)| json!({ "target": n, "met": ok })).collect::<Vec<_>>(),
    });
    std::fs::write(&out, serde_json::to_string_pretty(&report).unwrap()).unwrap();
    println!("{}", serde_json::to_string_pretty(&report).unwrap());
    if checks.iter().all(|(_, ok)| *ok) { 0 } else { 1 }
}

fn tempfile_dir() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("dsf-perf-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}
