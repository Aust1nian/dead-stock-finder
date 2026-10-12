//! Builds a scenario: the seller, the first sync's data, and each later
//! check's changes. Truth is never decided here; it is computed by
//! `truth::World` from the JSON this module emits.

mod later;
mod recipes;

use std::collections::{BTreeMap, HashMap, HashSet};

use chrono::{DateTime, Duration, NaiveDate, Offset, SecondsFormat, TimeZone, Timelike, Utc};
use chrono_tz::Tz;
use serde_json::{Map, Value, json};

use crate::rng::Rng;
use crate::truth::{World, present_at};
use crate::{Check, LABEL_KEY, Scenario, Size, Truth, WINDOWS};

const ZONES: [&str; 10] = [
    "America/New_York",
    "America/Chicago",
    "America/Denver",
    "America/Phoenix",
    "America/Los_Angeles",
    "America/Anchorage",
    "Pacific/Honolulu",
    "America/Toronto",
    "Europe/London",
    "Australia/Sydney",
];
const ADJECTIVES: [&str; 10] =
    ["Blue", "Red", "Cedar", "Linen", "Copper", "Wool", "Maple", "Sea Glass", "Amber", "Sage"];
const NOUNS: [&str; 10] =
    ["Mug", "Candle", "Scarf", "Card", "Tote", "Print", "Soap", "Ornament", "Journal", "Puzzle"];
const VARIATION_NAMES: [&str; 8] = ["Small", "Medium", "Large", "XL", "Red", "Green", "Gift Box", "Set of 2"];
const SEASONS: [&str; 4] = ["Spring", "Summer", "Fall", "Winter"];

pub(crate) fn ts(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn millis(ms: i64) -> DateTime<Utc> {
    DateTime::from_timestamp_millis(ms).expect("time in range")
}

struct Loc {
    id: String,
    zone: Tz,
    zone_name: &'static str,
    active: bool,
}

/// Where an item or variation is carried, as Square encodes it.
#[derive(Clone, Debug)]
enum Presence {
    /// Everywhere; `explicit` false leaves `present_at_all_locations` out.
    All {
        explicit: bool,
    },
    Except(Vec<usize>),
    Only(Vec<usize>),
}

struct ItemSpec {
    presence: Presence,
    vars: Vec<Presence>,
    archived: bool,
    weight: u64,
    pinned: bool,
    seller_value: bool,
}

struct VarModel {
    id: String,
    /// Location indices where the variation (and its item) is carried.
    carried: Vec<usize>,
    removed: bool,
}

struct ItemModel {
    id: String,
    num: usize,
    /// Random sales stop here (initial history only).
    cutoff: DateTime<Utc>,
    /// Edge-case items: random sales, edits, puts and deletes leave them be.
    pinned: bool,
    archived: bool,
    deleted: bool,
    vars: Vec<VarModel>,
}

impl ItemModel {
    fn live(&self) -> bool {
        !self.deleted && !self.archived
    }
}

enum State {
    Completed,
    Canceled,
    Open,
}

pub(crate) struct Builder {
    rng: Rng,
    size: Size,
    window: u32,
    /// Check times: [first, later...].
    ats: Vec<DateTime<Utc>>,
    locs: Vec<Loc>,
    main: usize,
    /// Effective selected locations, in merchant order.
    sel: Vec<usize>,
    /// Every location not selected (active or not).
    unsel: Vec<usize>,
    selected_out: Option<Vec<String>>,
    hs: DateTime<Utc>,
    world: World,
    items: Vec<ItemModel>,
    item_index: HashMap<String, usize>,
    categories: Vec<String>,
    seller_key: String,
    /// Orders by the check that first sees them (0 = the mock's seed).
    buckets: Vec<Vec<Value>>,
    used_close: HashSet<i64>,
    next_order: usize,
    /// Random completed orders that later checks may edit.
    editable: HashSet<String>,
    /// Random OPEN orders in the seed, by id.
    open_orders: Vec<Value>,
    cum_weight: Vec<u64>,
    cum_item: Vec<usize>,
    /// Edge-case item id -> what it covers (see `Scenario::cases`).
    cases: BTreeMap<String, String>,
}

pub(crate) fn build(seed: u64, size: Size) -> Scenario {
    let mut b = Builder::new(seed, size);
    b.catalog();
    b.recipes();
    b.random_history();
    let counts = b.counts();

    let initial = b.buckets[0].clone();
    b.world.learn_first_sync(&initial);
    let first_truth = b.world.truth(b.ats[0]);
    let catalog = b.seed_catalog(&first_truth);
    let mock = dsf_mock::Seed {
        app_id: dsf_mock::MOCK_APP_ID.to_string(),
        merchant: json!({
            "id": format!("M{seed:016X}"),
            "business_name": format!("Generated Gifts {seed}"),
            "main_location_id": b.locs[b.main].id,
            "currency": "USD",
        }),
        locations: b
            .locs
            .iter()
            .enumerate()
            .map(|(i, l)| {
                json!({
                    "id": l.id,
                    "name": format!("Shop {} ({})", i + 1, l.zone_name),
                    "timezone": l.zone_name,
                    "status": if l.active { "ACTIVE" } else { "INACTIVE" },
                    "currency": "USD",
                })
            })
            .collect(),
        catalog,
        orders: initial,
        counts,
    };
    let first = Check {
        at: b.ats[0],
        new_orders: Vec::new(),
        catalog_puts: Vec::new(),
        catalog_deletes: Vec::new(),
        label_tampering: Vec::new(),
        truth: first_truth,
    };
    let mut later = Vec::new();
    let mut previous = first.truth.clone();
    for k in 1..b.ats.len() {
        let check = b.later_check(k, &previous);
        previous = check.truth.clone();
        later.push(check);
    }
    Scenario {
        seed,
        window_days: b.window,
        selected_locations: b.selected_out.clone(),
        mock,
        first,
        later,
        cases: b.cases,
    }
}

impl Builder {
    fn new(seed: u64, size: Size) -> Builder {
        let mut rng = Rng::new(seed);
        let window = WINDOWS[(seed % 7) as usize];
        let later_checks = (seed / 7 % 5) as usize;

        // Locations in distinct zones (cycling if more than ten).
        let n = size.locations.max(1);
        let mut zones: Vec<&'static str> = ZONES.to_vec();
        rng.shuffle(&mut zones);
        let mut locs: Vec<Loc> = (0..n)
            .map(|i| {
                let zone_name = zones[i % zones.len()];
                Loc {
                    id: format!("L{}", i + 1),
                    zone: zone_name.parse().expect("zone"),
                    zone_name,
                    active: true,
                }
            })
            .collect();
        let main = if n >= 2 && rng.chance(20) { 1 } else { 0 };
        if n >= 2 && rng.chance(35) {
            let candidates: Vec<usize> = (0..n).filter(|i| *i != main).collect();
            let i = *rng.pick(&candidates);
            locs[i].active = false;
        }
        let active: Vec<usize> = (0..n).filter(|i| locs[*i].active).collect();
        let (sel, selected_out) = if active.len() >= 2 && rng.chance(45) {
            let mut pick: Vec<usize> = active.iter().copied().filter(|_| rng.chance(50)).collect();
            if pick.is_empty() || pick.len() == active.len() {
                pick = active.clone();
                let drop = rng.index(pick.len());
                pick.remove(drop);
            }
            let mut out: Vec<String> = pick.iter().map(|i| locs[*i].id.clone()).collect();
            rng.shuffle(&mut out);
            (pick, Some(out))
        } else if active.len() == 1 && rng.chance(20) {
            (active.clone(), Some(vec![locs[active[0]].id.clone()]))
        } else {
            (active.clone(), None)
        };
        let unsel: Vec<usize> = (0..n).filter(|i| !sel.contains(i)).collect();
        let zones: Vec<Tz> = locs.iter().map(|l| l.zone).collect();

        // The first check somewhere in 2025-2027; later ones 1 h to 120 d apart.
        let start = Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).single().expect("date");
        let mut t1 = start + Duration::milliseconds(rng.range(0, 3 * 365 * 86_400_000 - 1));
        t1 = away_from_midnight(t1, &zones);
        let mut ats = vec![t1];
        for _ in 0..later_checks {
            let gap_s = match rng.below(100) {
                0..20 => rng.range(3_600, 86_400),
                20..50 => rng.range(86_400, 14 * 86_400),
                _ => rng.range(15 * 86_400, 120 * 86_400),
            };
            let at = *ats.last().expect("one")
                + Duration::seconds(gap_s)
                + Duration::milliseconds(rng.range(0, 999));
            ats.push(away_from_midnight(at, &zones));
        }

        let world = World {
            window_days: window,
            t1,
            locations: Vec::new(),
            main_location_id: locs[main].id.clone(),
            selected: selected_out.clone(),
            items: BTreeMap::new(),
            first_seen: BTreeMap::new(),
            orders: BTreeMap::new(),
        };
        let hs = world.history_start();
        let seller_key = format!(
            "Square:{:08x}-{:04x}-4{:03x}-a{:03x}-{:012x}",
            rng.below(1 << 32),
            rng.below(1 << 16),
            rng.below(1 << 12),
            rng.below(1 << 12),
            rng.below(1 << 48)
        );
        let mut b = Builder {
            rng,
            size,
            window,
            buckets: vec![Vec::new(); ats.len()],
            ats,
            locs,
            main,
            sel,
            unsel,
            selected_out,
            hs,
            world,
            items: Vec::new(),
            item_index: HashMap::new(),
            categories: Vec::new(),
            seller_key,
            used_close: HashSet::new(),
            next_order: 0,
            editable: HashSet::new(),
            open_orders: Vec::new(),
            cum_weight: Vec::new(),
            cum_item: Vec::new(),
            cases: BTreeMap::new(),
        };
        b.world.locations = b
            .locs
            .iter()
            .map(|l| {
                json!({ "id": l.id, "timezone": l.zone_name, "status": if l.active { "ACTIVE" } else { "INACTIVE" } })
            })
            .collect();
        b
    }

    fn note(&mut self, item: usize, case: String) {
        self.cases.insert(self.items[item].id.clone(), case);
    }

    fn t1(&self) -> DateTime<Utc> {
        self.ats[0]
    }

    fn days(&self) -> i64 {
        i64::from(self.window)
    }

    // ---- catalog -------------------------------------------------------

    fn catalog(&mut self) {
        for c in 0..self.rng.range(2, 6) {
            self.categories.push(format!("C{:03}", c + 1));
        }
        let regular = if self.size.items >= 40 {
            self.size.items.saturating_sub(recipes::RECIPE_ITEMS)
        } else {
            self.size.items
        };
        for _ in 0..regular {
            let spec = self.regular_spec(true);
            self.add_item(spec, None);
        }
    }

    fn random_locations(&mut self) -> Vec<usize> {
        let n = self.locs.len();
        let mut out: Vec<usize> = (0..n).filter(|_| self.rng.chance(50)).collect();
        if out.is_empty() {
            out.push(self.rng.index(n));
        }
        out
    }

    fn regular_spec(&mut self, may_archive: bool) -> ItemSpec {
        let max = self.size.max_variations.max(1) as i64;
        let nvar = if max == 1 {
            1
        } else {
            match self.rng.below(100) {
                0..50 => 1,
                50..75 => 2,
                _ => self.rng.range(3.min(max), max),
            }
        } as usize;
        let n = self.locs.len();
        let roll = self.rng.below(100);
        let presence = match roll {
            0..75 => Presence::All { explicit: self.rng.chance(85) },
            // With one location, a few items are absent from it (carried nowhere).
            75..87 if n >= 2 || roll < 78 => Presence::Except(vec![self.rng.index(n)]),
            75..87 => Presence::All { explicit: true },
            _ => Presence::Only(self.random_locations()),
        };
        let vars = (0..nvar)
            .map(|_| match self.rng.below(100) {
                0..92 => Presence::All { explicit: true },
                92..97 => Presence::Except(vec![self.rng.index(n)]),
                _ => Presence::Only(self.random_locations()),
            })
            .collect();
        const WEIGHTS: [u64; 8] = [0, 1, 1, 2, 2, 3, 5, 8];
        ItemSpec {
            presence,
            vars,
            archived: may_archive && self.rng.chance(3),
            weight: *self.rng.pick(&WEIGHTS),
            pinned: false,
            seller_value: self.rng.chance(8),
        }
    }

    fn presence_fields(&self, obj: &mut Map<String, Value>, p: &Presence) {
        let ids = |v: &[usize]| -> Value { v.iter().map(|i| json!(self.locs[*i].id)).collect() };
        match p {
            Presence::All { explicit: true } => {
                obj.insert("present_at_all_locations".into(), json!(true));
            }
            Presence::All { explicit: false } => {}
            Presence::Except(v) => {
                obj.insert("present_at_all_locations".into(), json!(true));
                obj.insert("absent_at_location_ids".into(), ids(v));
            }
            Presence::Only(v) => {
                obj.insert("present_at_all_locations".into(), json!(false));
                obj.insert("present_at_location_ids".into(), ids(v));
            }
        }
    }

    fn variation_json(&mut self, item_id: &str, var_id: &str, k: usize, nvar: usize, p: &Presence) -> Value {
        let mut obj = Map::new();
        obj.insert("type".into(), json!("ITEM_VARIATION"));
        obj.insert("id".into(), json!(var_id));
        self.presence_fields(&mut obj, p);
        let mut data = Map::new();
        data.insert("item_id".into(), json!(item_id));
        let name = if nvar == 1 {
            "Regular".to_string()
        } else if k < VARIATION_NAMES.len() {
            VARIATION_NAMES[k].to_string()
        } else {
            format!("Option {}", k + 1)
        };
        data.insert("name".into(), json!(name));
        data.insert("sku".into(), json!(format!("SKU-{var_id}")));
        let price = self.rng.range(100, 20_000);
        if self.rng.chance(92) {
            data.insert("pricing_type".into(), json!("FIXED_PRICING"));
            data.insert("price_money".into(), json!({ "amount": price, "currency": "USD" }));
        } else {
            data.insert("pricing_type".into(), json!("VARIABLE_PRICING"));
        }
        data.insert("track_inventory".into(), json!(self.rng.chance(60)));
        if self.rng.chance(10) {
            let l = self.rng.index(self.locs.len());
            data.insert(
                "location_overrides".into(),
                json!([{ "location_id": self.locs[l].id, "track_inventory": self.rng.chance(50) }]),
            );
        }
        if self.rng.chance(30) {
            let cost = price * self.rng.range(30, 70) / 100;
            data.insert(
                "vendor_information".into(),
                json!([{ "unit_cost_money": { "amount": cost, "currency": "USD" } }]),
            );
        }
        obj.insert("item_variation_data".into(), Value::Object(data));
        Value::Object(obj)
    }

    /// Adds an item to the world (and the random-sale pool when it has a
    /// weight). `first_seen` is None for items present at the first check.
    fn add_item(&mut self, spec: ItemSpec, first_seen: Option<DateTime<Utc>>) -> usize {
        let num = self.items.len() + 1;
        let id = format!("I{num:06}");
        let mut obj = Map::new();
        obj.insert("type".into(), json!("ITEM"));
        obj.insert("id".into(), json!(id));
        self.presence_fields(&mut obj, &spec.presence);
        if spec.seller_value {
            let season = *self.rng.pick(&SEASONS);
            obj.insert(
                "custom_attribute_values".into(),
                json!({ self.seller_key.clone(): {
                    "name": "Season", "string_value": season, "key": self.seller_key, "type": "STRING"
                } }),
            );
        }
        let mut data = Map::new();
        let name = format!("{} {} {num}", self.rng.pick(&ADJECTIVES), self.rng.pick(&NOUNS));
        data.insert("name".into(), json!(name));
        if self.rng.chance(80) {
            let c = self.rng.pick(&self.categories).clone();
            data.insert("reporting_category".into(), json!({ "id": c }));
            data.insert("categories".into(), json!([{ "id": c }]));
        }
        if spec.archived {
            data.insert("is_archived".into(), json!(true));
        } else if self.rng.chance(30) {
            data.insert("is_archived".into(), json!(false));
        }
        data.insert("product_type".into(), json!("REGULAR"));
        let nvar = spec.vars.len();
        let vars: Vec<Value> = spec
            .vars
            .iter()
            .enumerate()
            .map(|(k, p)| self.variation_json(&id, &format!("V{num:06}-{:02}", k + 1), k, nvar, p))
            .collect();
        data.insert("variations".into(), Value::Array(vars));
        obj.insert("item_data".into(), Value::Object(data));
        let item = Value::Object(obj);

        let cutoff = self.random_cutoff();
        let vars = crate::truth::variations_of(&item)
            .iter()
            .map(|v| VarModel {
                id: v["id"].as_str().unwrap_or_default().to_string(),
                carried: self.carried(&item, v),
                removed: false,
            })
            .collect();
        if let Some(seen) = first_seen {
            self.world.first_seen.insert(id.clone(), seen);
            for v in crate::truth::variations_of(&item) {
                self.world.first_seen.insert(v["id"].as_str().unwrap_or_default().to_string(), seen);
            }
        }
        self.world.items.insert(id.clone(), item);
        let index = self.items.len();
        self.items.push(ItemModel {
            id: id.clone(),
            num,
            cutoff,
            pinned: spec.pinned,
            archived: spec.archived,
            deleted: false,
            vars,
        });
        self.item_index.insert(id, index);
        if spec.weight > 0 && !spec.pinned && !spec.archived {
            let total = self.cum_weight.last().copied().unwrap_or(0) + spec.weight;
            self.cum_weight.push(total);
            self.cum_item.push(index);
        }
        index
    }

    fn carried(&self, item: &Value, var: &Value) -> Vec<usize> {
        (0..self.locs.len())
            .filter(|i| present_at(item, &self.locs[*i].id) && present_at(var, &self.locs[*i].id))
            .collect()
    }

    /// When an item's random sales stop: spread so that some items are
    /// recent, some slow, some sold only before the history start.
    fn random_cutoff(&mut self) -> DateTime<Utc> {
        let t1 = self.t1();
        let hist_days = (t1 - self.hs).num_days();
        let w = self.days();
        let d = match self.rng.below(100) {
            0..35 => self.rng.range(0, w),
            35..70 => self.rng.range(w, hist_days - 4),
            70..80 => hist_days + self.rng.range(4, 55),
            _ => self.rng.range(0, hist_days - 4),
        };
        t1 - Duration::days(d) - Duration::seconds(self.rng.range(60, 86_400))
    }

    /// The seed's catalog: categories, this app's definition and pre-seeded
    /// labels (about half the seeds), and every item as of the first check.
    fn seed_catalog(&mut self, truth: &Truth) -> Vec<Value> {
        let mut catalog: Vec<Value> = self
            .categories
            .iter()
            .enumerate()
            .map(|(i, c)| json!({ "type": "CATEGORY", "id": c, "category_data": { "name": format!("Category {}", i + 1) } }))
            .collect();
        let mut items = self.world.items.clone();
        if self.rng.chance(50) {
            catalog.push(json!({
                "type": "CUSTOM_ATTRIBUTE_DEFINITION",
                "id": "DEF000001",
                "custom_attribute_definition_data": {
                    "type": "STRING",
                    "name": "Last sold",
                    "key": LABEL_KEY,
                    "allowed_object_types": ["ITEM", "ITEM_VARIATION"],
                    "seller_visibility": "SELLER_VISIBILITY_READ_WRITE_VALUES",
                    "source_application": { "application_id": dsf_mock::MOCK_APP_ID },
                },
            }));
            let ids: Vec<String> = truth.items.keys().cloned().collect();
            let count = self.rng.range(2, 6) as usize;
            for _ in 0..count.min(ids.len()) {
                let id = self.rng.pick(&ids).clone();
                let correct = truth.items.get(&id).cloned().flatten();
                let value = match self.rng.below(5) {
                    0 => "Last sold Mar 3, 2024".to_string(),
                    1 => "SLOW \u{b7} no sales in 12+ months".to_string(),
                    2 => correct.unwrap_or_else(|| "Last sold Jan 1, 2025".to_string()),
                    3 => "SLOW \u{b7} last sold Feb 29, 2024".to_string(),
                    _ => "checked by Dana".to_string(),
                };
                let item = items.get_mut(&id).expect("live item");
                set_label(item, Some(&value));
                let vars = crate::truth::variations_of(item).len();
                if self.rng.chance(50)
                    && let Some(v) = item.pointer_mut("/item_data/variations").and_then(Value::as_array_mut)
                {
                    // A stale variation label; on a single-variation item it
                    // must be removed.
                    let k = self.rng.index(vars);
                    set_label(&mut v[k], Some("Last sold Dec 24, 2023"));
                }
            }
        }
        catalog.extend(items.into_values());
        catalog
    }

    fn counts(&mut self) -> Vec<Value> {
        let mut out = Vec::new();
        let t1 = self.t1();
        for item in self.world.items.values() {
            for v in crate::truth::variations_of(item) {
                if v.pointer("/item_variation_data/track_inventory").and_then(Value::as_bool) != Some(true) {
                    continue;
                }
                let vid = v["id"].as_str().unwrap_or_default();
                let carried = self.carried(item, v);
                let Some(l) = carried.first() else { continue };
                let at = t1 - Duration::seconds(self.rng.range(60, 30 * 86_400));
                out.push(json!({
                    "catalog_object_id": vid,
                    "catalog_object_type": "ITEM_VARIATION",
                    "state": "IN_STOCK",
                    "location_id": self.locs[*l].id,
                    "quantity": self.rng.range(0, 40).to_string(),
                    "calculated_at": ts(at),
                }));
            }
        }
        out
    }

    // ---- orders --------------------------------------------------------

    fn pick_weighted(&mut self) -> Option<usize> {
        let total = *self.cum_weight.last()?;
        let r = self.rng.below(total);
        let pos = self.cum_weight.partition_point(|c| *c <= r);
        Some(self.cum_item[pos])
    }

    /// A live variation of `item` carried at `loc` (or anywhere), with a
    /// location where it is carried.
    fn sellable(&mut self, item: usize, loc: Option<usize>) -> Option<(String, usize)> {
        let it = &self.items[item];
        if !it.live() {
            return None;
        }
        let vars: Vec<usize> = (0..it.vars.len())
            .filter(|k| {
                let v = &it.vars[*k];
                !v.removed && loc.map_or(!v.carried.is_empty(), |l| v.carried.contains(&l))
            })
            .collect();
        if vars.is_empty() {
            return None;
        }
        let v = &self.items[item].vars[*self.rng.pick(&vars)];
        let id = v.id.clone();
        let l = match loc {
            Some(l) => l,
            None => *self.rng.pick(&v.carried.clone()),
        };
        Some((id, l))
    }

    fn quantity(&mut self) -> &'static str {
        match self.rng.below(100) {
            0..80 => "1",
            80..88 => "2",
            88..92 => "3",
            92..95 => "0.5",
            95..97 => "0",
            97..99 => "-1",
            _ => "0.25",
        }
    }

    fn line(&mut self, var: &str, qty: &str) -> Value {
        if self.rng.chance(10) {
            json!({ "catalog_object_id": var, "quantity": qty })
        } else {
            json!({ "catalog_object_id": var, "quantity": qty, "item_type": "ITEM" })
        }
    }

    fn custom_line() -> Value {
        json!({ "name": "Custom amount", "quantity": "1", "item_type": "CUSTOM_AMOUNT" })
    }

    fn gift_card_line() -> Value {
        json!({ "name": "Gift card", "quantity": "1", "item_type": "GIFT_CARD" })
    }

    /// A unique closing instant (no two orders share a millisecond).
    fn unique(&mut self, t: DateTime<Utc>) -> DateTime<Utc> {
        let mut ms = t.timestamp_millis();
        while !self.used_close.insert(ms) {
            ms += 1;
        }
        millis(ms)
    }

    /// The check that first sees an order closed at `t`.
    fn bucket_of(&self, t: DateTime<Utc>) -> Option<usize> {
        if t <= self.t1() - Duration::seconds(60) {
            return Some(0);
        }
        (1..self.ats.len())
            .find(|k| t > self.ats[k - 1] + Duration::seconds(1) && t <= self.ats[*k] - Duration::seconds(1))
    }

    fn updated_for(&mut self, bucket: usize, t: DateTime<Utc>) -> DateTime<Utc> {
        if bucket == 0 {
            if t < self.hs {
                return t;
            }
            let cap = self.t1() - Duration::seconds(1);
            return (t + Duration::seconds(self.rng.range(0, 2 * 86_400))).min(cap).max(t);
        }
        let cap = self.ats[bucket] - Duration::milliseconds(1);
        (t + Duration::seconds(self.rng.range(0, 6 * 3600))).min(cap).max(t)
    }

    fn new_order_id(&mut self) -> String {
        self.next_order += 1;
        format!("O{:07}", self.next_order)
    }

    #[allow(clippy::too_many_arguments)]
    fn order_json(
        &mut self,
        id: &str,
        loc: usize,
        state: &State,
        t: DateTime<Utc>,
        updated: DateTime<Utc>,
        lines: Vec<Value>,
        returns: Option<(String, Vec<Value>)>,
    ) -> Value {
        let mut o = Map::new();
        o.insert("id".into(), json!(id));
        o.insert("location_id".into(), json!(self.locs[loc].id));
        let state_text = match state {
            State::Completed => "COMPLETED",
            State::Canceled => "CANCELED",
            State::Open => "OPEN",
        };
        o.insert("state".into(), json!(state_text));
        if self.rng.chance(50) {
            o.insert("version".into(), json!(self.rng.range(1, 4)));
        }
        o.insert("line_items".into(), Value::Array(with_uids(id, "L", lines)));
        if let Some((source, lines)) = returns {
            o.insert(
                "returns".into(),
                json!([{
                    "uid": format!("{id}-R"),
                    "source_order_id": source,
                    "return_line_items": with_uids(id, "RL", lines),
                }]),
            );
        }
        let created = t - Duration::seconds(self.rng.range(1, 1800));
        o.insert("created_at".into(), json!(ts(created)));
        o.insert("updated_at".into(), json!(ts(updated)));
        if !matches!(state, State::Open) {
            o.insert("closed_at".into(), json!(ts(t)));
        }
        Value::Object(o)
    }

    /// Emits an order closed (or, if OPEN, created) at about `t` into the
    /// check that first sees it. None if `t` falls in no check's span.
    fn place(
        &mut self,
        loc: usize,
        state: State,
        t: DateTime<Utc>,
        lines: Vec<Value>,
        returns: Option<(String, Vec<Value>)>,
    ) -> Option<String> {
        let mut t = t;
        if (t - self.hs).num_seconds().abs() < 3 * 86_400 {
            t -= Duration::days(7);
        }
        let bucket = self.bucket_of(t)?;
        let t = self.unique(t);
        let updated = self.updated_for(bucket, t);
        let id = self.new_order_id();
        let order = self.order_json(&id, loc, &state, t, updated, lines, returns);
        self.buckets[bucket].push(order);
        Some(id)
    }

    /// One completed single-line sale (edge-case items).
    fn sale(&mut self, var: &str, loc: usize, t: DateTime<Utc>, qty: &str) -> Option<String> {
        let line = json!({ "catalog_object_id": var, "quantity": qty, "item_type": "ITEM" });
        self.place(loc, State::Completed, t, vec![line], None)
    }

    /// The seed's random order history.
    fn random_history(&mut self) {
        let lo = self.hs - Duration::days(60);
        let t1 = self.t1();
        for _ in 0..self.size.orders {
            let state = match self.rng.below(100) {
                0..92 => State::Completed,
                92..96 => State::Canceled,
                _ => State::Open,
            };
            let lead = self.pick_weighted().and_then(|it| self.sellable(it, None).map(|s| (it, s)));
            let Some((item, (var, loc))) = lead else {
                // Nothing sellable: a custom-amount sale.
                let loc = self.rng.index(self.locs.len());
                let t = lo + Duration::seconds(self.rng.range(0, (t1 - lo).num_seconds() - 120));
                self.place(loc, State::Completed, t, vec![Self::custom_line()], None);
                continue;
            };
            let cutoff = self.items[item].cutoff.min(t1 - Duration::seconds(120));
            let t = lo + Duration::seconds(self.rng.range(0, (cutoff - lo).num_seconds().max(0)));
            let qty = self.quantity();
            let mut lines = vec![self.line(&var, qty)];
            for _ in 0..2 {
                if !self.rng.chance(30) {
                    continue;
                }
                for _ in 0..4 {
                    let Some(other) = self.pick_weighted() else { break };
                    if self.items[other].cutoff < t {
                        continue;
                    }
                    if let Some((v, _)) = self.sellable(other, Some(loc)) {
                        let qty = self.quantity();
                        lines.push(self.line(&v, qty));
                        break;
                    }
                }
            }
            if self.rng.chance(5) {
                lines.push(Self::custom_line());
            }
            if self.rng.chance(2) {
                lines.push(Self::gift_card_line());
            }
            let completed = matches!(state, State::Completed);
            let open = matches!(state, State::Open);
            let Some(id) = self.place(loc, state, t, lines, None) else { continue };
            if open {
                let order = self.buckets[0].last().cloned().expect("just placed");
                self.open_orders.push(order);
            }
            if !completed {
                continue;
            }
            self.editable.insert(id.clone());
            // A later return of the first line (never moves a date), or an
            // exchange that returns it and sells something else.
            let roll = self.rng.below(100);
            if roll < 4 {
                let later = t + Duration::seconds(self.rng.range(3600, 20 * 86_400));
                if later < t1 - Duration::hours(1) {
                    let back = json!({ "catalog_object_id": var, "quantity": "1", "item_type": "ITEM" });
                    if roll < 3 {
                        self.place(loc, State::Completed, later, Vec::new(), Some((id, vec![back])));
                    } else if let Some(other) = self.pick_weighted()
                        && let Some((v, _)) = self.sellable(other, Some(loc))
                    {
                        let sold = self.line(&v, "1");
                        self.place(loc, State::Completed, later, vec![sold], Some((id, vec![back])));
                    }
                }
            }
        }
    }
}

fn with_uids(order: &str, tag: &str, lines: Vec<Value>) -> Vec<Value> {
    lines
        .into_iter()
        .enumerate()
        .map(|(k, line)| {
            let mut m = Map::new();
            m.insert("uid".into(), json!(format!("{order}-{tag}{}", k + 1)));
            if let Value::Object(rest) = line {
                m.extend(rest);
            }
            Value::Object(m)
        })
        .collect()
}

/// Sets (or with None removes) this app's label on one object.
fn set_label(object: &mut Value, text: Option<&str>) {
    let Some(obj) = object.as_object_mut() else { return };
    match text {
        Some(text) => {
            let values = obj.entry("custom_attribute_values").or_insert_with(|| json!({}));
            values[LABEL_KEY] =
                json!({ "name": "Last sold", "string_value": text, "key": LABEL_KEY, "type": "STRING" });
        }
        None => {
            if let Some(values) = obj.get_mut("custom_attribute_values").and_then(Value::as_object_mut) {
                values.shift_remove(LABEL_KEY);
                if values.is_empty() {
                    obj.shift_remove("custom_attribute_values");
                }
            }
        }
    }
}

/// Moves a check time out of the 10 minutes either side of midnight in every
/// location's zone, so "today" and "added" dates can't hinge on a few
/// seconds (a check repeated a minute later sees the same dates).
fn away_from_midnight(mut t: DateTime<Utc>, zones: &[Tz]) -> DateTime<Utc> {
    for _ in 0..50 {
        let near = zones.iter().any(|z| {
            let s = t.with_timezone(z).num_seconds_from_midnight();
            !(600..=86_400 - 600).contains(&s)
        });
        if !near {
            break;
        }
        t += Duration::minutes(25);
    }
    t
}

/// The UTC instant of a local wall-clock time (later by an hour if that time
/// falls in a daylight-saving gap; the earlier of two if repeated).
fn local_instant(zone: Tz, date: NaiveDate, h: u32, m: u32, s: u32) -> DateTime<Utc> {
    let mut naive = date.and_hms_opt(h, m, s).expect("valid time");
    for _ in 0..4 {
        if let Some(t) = zone.from_local_datetime(&naive).earliest() {
            return t.with_timezone(&Utc);
        }
        naive += Duration::hours(1);
    }
    Utc.from_utc_datetime(&naive)
}

fn local_date(zone: Tz, t: DateTime<Utc>) -> NaiveDate {
    t.with_timezone(&zone).date_naive()
}

fn offset_secs(zone: Tz, t: DateTime<Utc>) -> i32 {
    t.with_timezone(&zone).offset().fix().local_minus_utc()
}
