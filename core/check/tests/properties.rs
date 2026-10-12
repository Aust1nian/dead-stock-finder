//! Generator and property tests (spec § Tests): every seed builds a catalog
//! and a sales history with known answers, and these must hold for all:
//!
//! - every label equals the truth computed by the generator (which never
//!   uses this app's code), after the first check and after each later one;
//! - returns never move a last-sold date (the truth ignores them);
//! - a second check with no new orders writes nothing;
//! - every outgoing write differs from the fetched object only in this app's
//!   key (the mock audits each one);
//! - killing the check at any point and running it again gives the same
//!   final labels;
//! - no request goes to a host outside the allowlist;
//! - tokens never appear in logs, the cache, exports or diagnostics.
//!
//! Seeds: DSF_SEEDS, e.g. "0-199" (every push, the default) or "0-1999"
//! (nightly).

mod common;

use std::collections::BTreeMap;

use chrono::Duration;
use common::Harness;
use dsf_check::Outcome;
use dsf_compute::LABEL_KEY;
use dsf_schedule::Trigger;
use dsf_sync::Settings;
use serde_json::json;

fn seeds() -> Vec<u64> {
    let spec = std::env::var("DSF_SEEDS").unwrap_or_else(|_| "0-199".into());
    let mut out = Vec::new();
    for part in spec.split(',') {
        match part.split_once('-') {
            Some((a, b)) => out.extend(a.trim().parse::<u64>().unwrap()..=b.trim().parse::<u64>().unwrap()),
            None => out.push(part.trim().parse().unwrap()),
        }
    }
    out
}

fn harness(s: &dsf_gen::Scenario) -> Harness {
    let settings = Settings {
        window_days: s.window_days,
        locations: s.selected_locations.clone(),
        labels_on: true,
        ..Settings::default()
    };
    Harness::new(s.mock.clone(), settings, s.first.at)
}

/// Labels in the mock that differ from the truth.
fn mismatches(h: &Harness, truth: &dsf_gen::Truth) -> Vec<String> {
    let state = h.mock.state();
    let mut out = Vec::new();
    let all: BTreeMap<&String, &Option<String>> = truth.items.iter().chain(truth.variations.iter()).collect();
    for (id, want) in all {
        let got = state.label_of(id);
        if got.as_ref() != want.as_ref() {
            out.push(format!("{id}: expected {want:?}, got {got:?}"));
        }
    }
    out
}

fn done(o: &Outcome, seed: u64, what: &str) {
    assert!(matches!(o, Outcome::Done(_)), "seed {seed}: {what} didn't finish: {o:?}");
}

fn apply(h: &Harness, check: &dsf_gen::Check) {
    h.set_now(check.at);
    let mut state = h.mock.state();
    state.add_orders(check.new_orders.iter().cloned());
    for object in &check.catalog_puts {
        state.put_object(object.clone());
    }
    for id in &check.catalog_deletes {
        state.delete_object(id);
    }
    for (id, value) in &check.label_tampering {
        state.staff_edit(id, |o| match value {
            Some(text) => {
                if o.get("custom_attribute_values").is_none() {
                    o["custom_attribute_values"] = json!({});
                }
                o["custom_attribute_values"][LABEL_KEY] =
                    json!({ "name": "Last sold", "string_value": text, "key": LABEL_KEY, "type": "STRING" });
            }
            None => {
                if let Some(map) = o.get_mut("custom_attribute_values").and_then(|m| m.as_object_mut()) {
                    map.shift_remove(LABEL_KEY);
                }
            }
        });
    }
}

fn check_seed(seed: u64) {
    let s = dsf_gen::scenario(seed);
    let h = harness(&s);
    done(&h.check(Trigger::Manual), seed, "first check");
    let wrong = mismatches(&h, &s.first.truth);
    assert!(
        wrong.is_empty(),
        "seed {seed}, first check: {} wrong labels:\n{}",
        wrong.len(),
        wrong.join("\n")
    );
    let violations = h.violations();
    assert!(violations.is_empty(), "seed {seed}: {}", violations.join("\n"));

    // A second check with no new orders writes nothing.
    let writes = h.writes();
    h.advance(Duration::minutes(1));
    done(&h.check(Trigger::Manual), seed, "second check");
    assert_eq!(h.writes(), writes, "seed {seed}: the second check wrote");

    for (n, later) in s.later.iter().enumerate() {
        apply(&h, later);
        done(&h.check(Trigger::Manual), seed, &format!("later check {n}"));
        let wrong = mismatches(&h, &later.truth);
        assert!(
            wrong.is_empty(),
            "seed {seed}, later check {n}: {} wrong labels:\n{}",
            wrong.len(),
            wrong.join("\n")
        );
    }
    let violations = h.violations();
    assert!(violations.is_empty(), "seed {seed}: {}", violations.join("\n"));

    // No request outside the allowlist.
    let base = format!("http://{}/", h.mock.addr());
    for r in h.requests.lock().unwrap().iter() {
        assert!(r.allowed && r.url.starts_with(&base), "seed {seed}: request to {}", r.url);
    }
    // No token in any file the app wrote, including diagnostics.
    let diagnostics = dsf_check::diagnostics::build(h.dir.path(), "test", "mock");
    std::fs::write(h.dir.path().join("diagnostics.txt"), diagnostics).unwrap();
    let report =
        dsf_sync::report(&h.cache(), &h.cache().settings().unwrap(), s.first.at, &Default::default())
            .unwrap();
    dsf_sync::export::write_csv(&report, &h.dir.path().join("export.csv")).unwrap();
    let tokens = h.tokens().unwrap();
    for file in walk(h.dir.path()) {
        let text = String::from_utf8_lossy(&std::fs::read(&file).unwrap()).to_string();
        assert!(
            !text.contains(&tokens.access_token)
                && !text.contains(&tokens.refresh_token)
                && !text.contains("EAAAmock"),
            "seed {seed}: a token in {}",
            file.display()
        );
    }
}

fn walk(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() { out.extend(walk(&p)) } else { out.push(p) }
    }
    out
}

/// Interrupted at a request chosen by the seed, then run again: the same
/// final labels as an uninterrupted run.
fn interrupted_seed(seed: u64) {
    let s = dsf_gen::scenario(seed);
    let clean = harness(&s);
    done(&clean.check(Trigger::Manual), seed, "clean check");
    let total = clean.mock.state().log.len() as u64;
    let cut = 1 + (seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 33) % total.max(1);
    let h = harness(&s);
    h.mock.state().faults.drop_from = Some(cut);
    h.mock.state().faults.drop_count = 1_000_000;
    let _ = h.check(Trigger::Manual);
    h.mock.state().faults.drop_from = None;
    done(&h.check(Trigger::Manual), seed, "rerun after interruption");
    let wrong = mismatches(&h, &s.first.truth);
    assert!(wrong.is_empty(), "seed {seed}, cut at request {cut} of {total}: {}", wrong.join("\n"));
    assert!(h.violations().is_empty(), "seed {seed}: {}", h.violations().join("\n"));
}

#[test]
fn every_seed_matches_the_truth() {
    let seeds = seeds();
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).min(16);
    let chunks: Vec<Vec<u64>> =
        (0..threads).map(|t| seeds.iter().copied().skip(t).step_by(threads).collect()).collect();
    std::thread::scope(|scope| {
        let handles: Vec<_> = chunks
            .into_iter()
            .map(|chunk| scope.spawn(move || chunk.into_iter().for_each(check_seed)))
            .collect();
        for h in handles {
            h.join().expect("a seed failed");
        }
    });
    eprintln!("{} seeds matched the truth", seeds.len());
}

#[test]
fn interrupted_checks_end_the_same() {
    let seeds: Vec<u64> = seeds().into_iter().filter(|s| s % 5 == 0).collect();
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).min(16);
    let chunks: Vec<Vec<u64>> =
        (0..threads).map(|t| seeds.iter().copied().skip(t).step_by(threads).collect()).collect();
    std::thread::scope(|scope| {
        let handles: Vec<_> = chunks
            .into_iter()
            .map(|chunk| scope.spawn(move || chunk.into_iter().for_each(interrupted_seed)))
            .collect();
        for h in handles {
            h.join().expect("a seed failed");
        }
    });
}
