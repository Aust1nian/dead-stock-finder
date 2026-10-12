//! A small gift shop for UI tests and local runs against the mock: two
//! locations, a few dozen items, fourteen months of sales ending today.

use chrono::{DateTime, Duration, Utc};
use dsf_mock::Seed;
use serde_json::{Value, json};

const ITEMS: &[(&str, &str, &[&str], i64)] = &[
    ("Beeswax candle", "Home", &["Small", "Large"], 1800),
    ("Ceramic mug", "Kitchen", &["Blue", "Red", "Speckled"], 2400),
    ("Linen tea towel", "Kitchen", &["Natural"], 1600),
    ("Wool throw", "Home", &["Grey", "Oat"], 12800),
    ("Letterpress card", "Paper", &["Birthday", "Thank you", "Blank"], 600),
    ("Brass bottle opener", "Bar", &["One size"], 2200),
    ("Olive wood board", "Kitchen", &["Small", "Medium", "Large"], 4600),
    ("Hand cream", "Bath", &["Lavender", "Unscented"], 1400),
    ("Pocket notebook", "Paper", &["Lined", "Dot", "Plain"], 900),
    ("Enamel pin", "Accessories", &["Fern", "Moth"], 1200),
    ("Silk scarf", "Accessories", &["Indigo"], 8800),
    ("Cast iron trivet", "Kitchen", &["Black"], 3200),
    ("Soy candle tin", "Home", &["Cedar", "Fig", "Sea salt"], 1500),
    ("Wall calendar", "Paper", &["2026"], 2800),
    ("Cocktail shaker", "Bar", &["Steel", "Copper"], 4200),
    ("Bath salts", "Bath", &["Eucalyptus"], 1900),
    ("Woven basket", "Home", &["Small", "Large"], 5400),
    ("Leather keyring", "Accessories", &["Tan", "Black"], 1800),
];

/// Deterministic mixing for the demo data.
fn mix(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

fn ts(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

pub fn seed() -> (Seed, DateTime<Utc>) {
    let now = Utc::now();
    let mut categories: Vec<&str> = ITEMS.iter().map(|i| i.1).collect();
    categories.sort();
    categories.dedup();
    let mut catalog: Vec<Value> = categories
        .iter()
        .map(|c| json!({ "type": "CATEGORY", "id": format!("CAT-{c}"), "category_data": { "name": c } }))
        .collect();
    let mut orders = Vec::new();
    let mut counts = Vec::new();
    let mut n = 0u64;
    for (i, (name, category, variations, price)) in ITEMS.iter().enumerate() {
        let item_id = format!("DEMO-I{i:02}");
        let vars: Vec<Value> = variations
            .iter()
            .enumerate()
            .map(|(j, v)| {
                json!({ "type": "ITEM_VARIATION", "id": format!("DEMO-V{i:02}-{j}"), "item_variation_data": {
                    "name": v, "sku": format!("GS-{i:02}{j}"), "pricing_type": "FIXED_PRICING",
                    "price_money": { "amount": price, "currency": "USD" }, "track_inventory": true,
                    "vendor_information": [{ "unit_cost_money": { "amount": price * 45 / 100, "currency": "USD" } }] } })
            })
            .collect();
        catalog.push(json!({ "type": "ITEM", "id": item_id, "item_data": {
            "name": name, "reporting_category": { "id": format!("CAT-{category}") }, "variations": vars } }));
        for j in 0..variations.len() {
            let var = format!("DEMO-V{i:02}-{j}");
            let r = mix((i * 10 + j) as u64);
            // Some sell weekly, some stopped selling months ago, a few never.
            let last_sale_days = match r % 6 {
                0 => None,
                1 => Some(150 + (r >> 8) % 200),
                2 => Some(95 + (r >> 8) % 40),
                _ => Some((r >> 8) % 60),
            };
            if let Some(last) = last_sale_days {
                let mut day = last as i64;
                while day < 420 {
                    n += 1;
                    let location = if mix(n).is_multiple_of(3) { "DEMO-L2" } else { "DEMO-L1" };
                    let closed = now - Duration::days(day) - Duration::minutes((mix(n) % 600) as i64);
                    orders.push(json!({ "id": format!("DEMO-O{n:05}"), "location_id": location, "state": "COMPLETED",
                        "closed_at": ts(closed), "updated_at": ts(closed),
                        "line_items": [{ "uid": "1", "catalog_object_id": var, "quantity": "1", "item_type": "ITEM" }] }));
                    day += 3 + (mix(n + 7) % 25) as i64;
                }
            }
            counts.push(json!({ "catalog_object_id": var, "catalog_object_type": "ITEM_VARIATION", "state": "IN_STOCK",
                "location_id": "DEMO-L1", "quantity": ((r >> 16) % 14).to_string(), "calculated_at": ts(now - Duration::hours(2)) }));
        }
    }
    let seed = Seed {
        app_id: String::new(),
        merchant: json!({ "id": "DEMO-M", "business_name": "Main St Boutique", "main_location_id": "DEMO-L1", "currency": "USD" }),
        locations: vec![
            json!({ "id": "DEMO-L1", "name": "Main St", "timezone": "America/New_York", "status": "ACTIVE" }),
            json!({ "id": "DEMO-L2", "name": "Farmers market", "timezone": "America/New_York", "status": "ACTIVE" }),
        ],
        catalog,
        orders,
        counts,
    };
    (seed, now)
}
