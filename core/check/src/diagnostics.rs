//! Help → Save diagnostics: what support needs to see why a check failed,
//! with item names, amounts and ids replaced by placeholders. The window
//! shows the text before it is saved.

use std::collections::BTreeSet;
use std::path::Path;

use dsf_sync::Cache;

const LOG_LINES: usize = 300;

/// Item and variation names and ids from the cache, longest first, so a
/// name containing another is replaced whole.
fn secrets(cache: &Cache) -> (Vec<String>, Vec<String>) {
    let conn = cache.conn();
    let collect = |sql: &str| -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        if let Ok(mut stmt) = conn.prepare(sql)
            && let Ok(rows) = stmt.query_map([], |r| r.get::<_, Option<String>>(0))
        {
            for v in rows.flatten().flatten() {
                if v.trim().len() >= 2 {
                    out.insert(v);
                }
            }
        }
        out
    };
    let mut names: Vec<String> = collect("SELECT name FROM items")
        .into_iter()
        .chain(collect("SELECT name FROM variations"))
        .chain(collect("SELECT name FROM categories"))
        .chain(collect("SELECT name FROM locations"))
        .chain(collect("SELECT value FROM meta WHERE key = 'business_name'"))
        .chain(collect("SELECT sku FROM variations"))
        .collect();
    names.sort_by_key(|n| std::cmp::Reverse(n.len()));
    let mut ids: Vec<String> = collect("SELECT id FROM items")
        .into_iter()
        .chain(collect("SELECT id FROM variations"))
        .chain(collect("SELECT id FROM locations"))
        .chain(collect("SELECT id FROM orders"))
        .chain(collect(
            "SELECT value FROM meta WHERE key IN ('merchant_id', 'definition_id', 'main_location_id')",
        ))
        .collect();
    ids.sort_by_key(|n| std::cmp::Reverse(n.len()));
    (names, ids)
}

/// Replaces names, ids, money amounts and anything token-shaped.
pub fn redact(text: &str, names: &[String], ids: &[String]) -> String {
    let mut out = text.to_string();
    for id in ids {
        out = out.replace(id.as_str(), "<id>");
    }
    for name in names {
        out = out.replace(name.as_str(), "<name>");
    }
    redact_patterns(&out)
}

fn redact_patterns(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for word in text.split_inclusive(|c: char| c.is_whitespace() || c == '"' || c == ',') {
        let trimmed = word.trim_end_matches(|c: char| c.is_whitespace() || c == '"' || c == ',');
        let tail = &word[trimmed.len()..];
        let token_like = ["EAAA", "EQAA", "sq0", "sandbox-sq0"].iter().any(|p| trimmed.starts_with(p))
            && trimmed.len() > 20;
        let money = trimmed.trim_start_matches('$').replace(',', "");
        let is_money = trimmed.starts_with('$') && money.parse::<f64>().is_ok();
        let long_id = trimmed.len() >= 16
            && trimmed.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            && trimmed.chars().any(|c| c.is_ascii_digit())
            && trimmed.chars().any(|c| c.is_ascii_uppercase());
        if token_like {
            out.push_str("<secret>");
        } else if is_money {
            out.push_str("<amount>");
        } else if long_id {
            out.push_str("<id>");
        } else {
            out.push_str(trimmed);
        }
        out.push_str(tail);
    }
    out
}

/// The diagnostics text, already redacted.
pub fn build(data_dir: &Path, registration: &str, environment: &str) -> String {
    let mut lines = Vec::new();
    lines.push("Idle Stock Finder diagnostics (item names, amounts and ids replaced)".to_string());
    lines.push(format!(
        "app {} · Square-Version {} · {}",
        env!("CARGO_PKG_VERSION"),
        dsf_square::SQUARE_VERSION,
        environment
    ));
    lines.push(format!("system {} {}", std::env::consts::OS, std::env::consts::ARCH));
    lines.push(format!("background check: {registration}"));
    let cache = Cache::open(&super::paths::cache_file(data_dir)).ok();
    let (names, ids) = cache.as_ref().map(secrets).unwrap_or_default();
    if let Some(cache) = &cache {
        if let Ok(s) = cache.settings() {
            lines.push(format!(
                "settings: window {} days · {} · labels {} · check {:02}:{:02} · background {} · setup {}",
                s.window_days,
                s.locations
                    .as_ref()
                    .map(|l| format!("{} locations selected", l.len()))
                    .unwrap_or_else(|| "all active locations".into()),
                if s.labels_on { "on" } else { "off" },
                s.check_time.0,
                s.check_time.1,
                if s.background_on { "on" } else { "off" },
                if s.setup_done { "done" } else { "not done" },
            ));
        }
        let count = |sql: &str| cache.conn().query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap_or(-1);
        lines.push(format!(
            "cache: {} items · {} variations · {} orders · {} sale lines · {} stock counts · {} locations",
            count("SELECT COUNT(*) FROM items"),
            count("SELECT COUNT(*) FROM variations"),
            count("SELECT COUNT(*) FROM orders"),
            count("SELECT COUNT(*) FROM sale_lines"),
            count("SELECT COUNT(*) FROM inventory"),
            count("SELECT COUNT(*) FROM locations"),
        ));
        let status = super::status(cache);
        lines.push(format!(
            "status: synced {} · disconnected {} · labels on {} items · failures in a row {} · stopped {}",
            status.synced,
            status.disconnected,
            status.labels_in_square,
            status.failure_count,
            status
                .halted
                .as_ref()
                .map(|h| format!("since {} at {}", h.at, h.paths.join(" ")))
                .unwrap_or_else(|| "no".into())
        ));
        if let Ok(mut stmt) = cache
            .conn()
            .prepare("SELECT started_at, ended_at, trigger, outcome, COALESCE(detail, ''), orders_read, labels_written, labels_skipped FROM checks ORDER BY id DESC LIMIT 30")
        {
            lines.push("recent checks:".into());
            if let Ok(rows) = stmt.query_map([], |r| {
                Ok(format!(
                    "  {} → {} {} {} {} · orders {} · written {} · skipped {}",
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, i64>(5)?,
                    r.get::<_, i64>(6)?,
                    r.get::<_, i64>(7)?
                ))
            }) {
                lines.extend(rows.flatten());
            }
        }
    }
    let log = std::fs::read_to_string(super::paths::log_dir(data_dir).join("check.log")).unwrap_or_default();
    let recent: Vec<&str> = log.lines().rev().take(LOG_LINES).collect();
    lines.push(format!("log (last {} lines):", recent.len()));
    lines.extend(recent.into_iter().rev().map(|l| format!("  {l}")));
    redact(&lines.join("\n"), &names, &ids)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_names_ids_amounts_and_tokens() {
        let names = vec!["Blue Mug, large".to_string(), "Mug".to_string()];
        let ids = vec!["VKX3J7Q2ABCDEF12".to_string()];
        let text = "skipped VKX3J7Q2ABCDEF12 (Blue Mug, large): price $1,200.00 token EAAAl-abcdefghijklmnopqrstuvwxyz and ZZ9QWERTYUIOP12345";
        let out = redact(text, &names, &ids);
        assert!(!out.contains("Mug"), "{out}");
        assert!(!out.contains("VKX3J7Q2"), "{out}");
        assert!(!out.contains("1,200"), "{out}");
        assert!(!out.contains("EAAAl"), "{out}");
        assert!(!out.contains("ZZ9QWERTY"), "{out}");
        assert!(
            out.contains("<name>")
                && out.contains("<id>")
                && out.contains("<amount>")
                && out.contains("<secret>"),
            "{out}"
        );
    }
}
