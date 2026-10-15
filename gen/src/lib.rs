//! Seeded generator of catalogs and sales histories with known answers.
//!
//! [`scenario`] turns a seed into a Square seller (merchant, locations,
//! catalog, orders, inventory counts) for [`dsf_mock::MockSquare::start`],
//! a first check and 0–4 later checks. Each [`Check`] lists what changes in
//! Square just before it and the labels Square must hold just after it
//! ([`Truth`]). The truth is computed here, from the same JSON the mock
//! serves, by an implementation of the last-sold rules that shares no code
//! with the app (see `truth.rs`). Same seed, byte-identical output: no wall
//! clock, no hash-ordered output, a private PRNG.
//!
//! # Applying a check
//!
//! Before each later check, with the mock's clock set to `check.at`, apply in
//! this order: `new_orders` ([`dsf_mock::State::add_orders`]), `catalog_puts`
//! ([`dsf_mock::State::put_object`]), `catalog_deletes`, then
//! `label_tampering` (a staff edit setting or removing [`LABEL_KEY`]'s
//! `string_value`). Then run the app's check with its clock at `check.at`
//! and compare every id in `truth` with [`dsf_mock::State::label_of`].
//! No tampered id is also put or deleted in the same check.
//!
//! # The rules the truth implements
//!
//! History read: M = [`months_for_window`]; history start = first check
//! minus M calendar months. Known orders: completed orders closed at or
//! after the history start, plus every later `new_orders` entry; the newest
//! `updated_at` of an id replaces older versions. A sale is a line on a
//! COMPLETED order at a selected location with a catalog id, an item type
//! other than CUSTOM_AMOUNT, and a quantity above zero. Dates are calendar
//! dates in the selling location's zone; slow is more than `window_days`
//! days. No sale: "no sales since added" (home zone) for objects first
//! seen after the first check, else "no sales in M+ months". Items always
//! get a label; variations only when their item has 2+ live variations;
//! anything carried at no selected location gets none; archived items are
//! left out.
//!
//! # Seed axes (the same for [`scenario`] and [`scenario_sized`])
//!
//! - `window_days` = `[30, 60, 90, 120, 180, 365, 400][seed % 7]`
//!   (365 gives M = 14, 400 gives M = 15; the rest M = 12).
//! - Later checks: `(seed / 7) % 5` (0–4), each 1 hour to 120 days after
//!   the one before; the first check falls in 2025–2027.
//! - Locations ([`scenario`] only): `1 + (seed / 35) % 4`, in distinct zones
//!   from ten (New York, Chicago, Denver, Phoenix, Los Angeles, Anchorage,
//!   Honolulu, Toronto, London, Sydney). With 2+ locations: sometimes one
//!   is INACTIVE, the main location is sometimes not the first, and
//!   `selected_locations` is sometimes a shuffled subset of the active ones
//!   (which may leave out the main location, so "added" dates use the first
//!   selected location's zone).
//!
//! # Edge cases (every seed, where its locations allow)
//!
//! Each case below is a dedicated item that random sales never touch.
//! - Sales exactly `window_days` and `window_days + 1` days before a check
//!   (in the selling zone), at a time of day just after the check's, so
//!   elapsed hours and calendar days disagree.
//! - A late-night sale at the westernmost selected location whose UTC date
//!   differs from the local date, on the boundary day (23:20+ local west of
//!   UTC, just after midnight east of it; not at UTC+0).
//! - A boundary sale with a daylight-saving change between it and the
//!   check, when a selected zone observes DST and some check's window spans
//!   a change (about 45% of seeds).
//! - Two selected locations in different zones where the later sale (by
//!   instant) is at the western one but has the earlier local date (seeds
//!   with 2+ selected locations; about a third of seeds).
//! - Never sold; sold only before the history start ("no sales in M+").
//! - A recent sale at an unselected or INACTIVE location over an old one at
//!   a selected location, and an item carried only at unselected locations
//!   (no label), whenever some location is not selected; an item carried at
//!   only some locations (2+ locations).
//! - Variations absent at every selected location (no label) and at one of
//!   several selected locations.
//! - Recent CANCELED and OPEN orders with catalog lines; quantities "0" and
//!   "-1" (never count) and "0.25" (counts); a CUSTOM_AMOUNT line that names
//!   a catalog variation (never counts); gift-card lines.
//! - A return order (empty `line_items`, `returns`) after the last sale,
//!   and an exchange (returns one item, sells another).
//! - About 30% of seeds: one item with 50 variations.
//! - About half the seeds: pre-seeded labels under [`LABEL_KEY`] (stale,
//!   wrong, already right, or on objects that must have none) with this
//!   app's CUSTOM_ATTRIBUTE_DEFINITION in the catalog.
//!
//! Random items add: 1 to `max_variations` variations, partial presence,
//! archived items, seller `Square:<uuid>` attribute values, orders without
//! `version`, quantities 2, 3 and 0.5, custom-amount and gift-card lines,
//! canceled/open orders, returns, exchanges, orders before the history
//! start (`updated_at` = `closed_at`), and sales at every location.
//!
//! # Later checks
//!
//! Each adds new sales (including ones that bring slow items back inside
//! the window); time advances so items cross into slow; 1–3 orders closed
//! before the check are re-sent with a newer `updated_at` and a line
//! removed, zeroed or switched to another variation; an OPEN order is
//! re-sent COMPLETED; an order closed before the previous check arrives
//! late (updated after it); new items and new variations appear (some sold,
//! some "no sales since added", slow once older than the window); a
//! variation is removed (sometimes leaving one, so its variation label must
//! go); items are renamed, archived and deleted; staff hand-edit or remove
//! labels, or add one where none belongs. Puts of existing items keep the
//! labels Square held (the previous truth) and seller values.
//!
//! # Avoided on purpose (so the truth is unambiguous)
//!
//! No sale within 2 days of the history start; sales only where the
//! variation is carried; no two orders closed at the same millisecond;
//! presence never changes after the first check; a re-sent order is always
//! COMPLETED at its original location; puts and removals always keep a
//! variation that existed when the item was first seen; check times are
//! never within 10 minutes of midnight in any location's zone (a
//! check repeated a minute later sees the same dates); archived items
//! never get pre-seeded labels.

mod build;
mod rng;
mod truth;

#[cfg(test)]
mod tests;

use std::collections::BTreeMap;

use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::{Value, json};

pub use truth::months_for_window;

/// The app's label key in `custom_attribute_values` (must equal the app's).
pub const LABEL_KEY: &str = "idle_stock_last_sold";

/// The windows a seed can draw, by `seed % 7`.
pub const WINDOWS: [u32; 7] = [30, 60, 90, 120, 180, 365, 400];

/// How much to generate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Size {
    pub items: usize,
    pub max_variations: usize,
    pub orders: usize,
    pub locations: usize,
}

impl Size {
    /// The largest small scenario: 120 items, up to 8 variations, 1,500
    /// orders, 4 locations. [`scenario`] draws a size up to this per seed.
    pub fn small() -> Size {
        Size { items: 120, max_variations: 8, orders: 1500, locations: 4 }
    }

    /// The size [`scenario`] uses for `seed`: 40–120 items, 1–8 variations,
    /// 300–1,500 orders, `1 + (seed / 35) % 4` locations.
    pub fn small_for(seed: u64) -> Size {
        let mut rng = rng::Rng::new(seed ^ 0x5157_E5EE_D512_E000);
        let max = Size::small();
        Size {
            items: rng.range(40, max.items as i64) as usize,
            max_variations: rng.range(1, max.max_variations as i64) as usize,
            orders: rng.range(300, max.orders as i64) as usize,
            locations: 1 + (seed / 35 % 4) as usize,
        }
    }
}

/// A seller and a sequence of checks with known answers.
#[derive(Clone, Debug)]
pub struct Scenario {
    pub seed: u64,
    pub window_days: u32,
    /// None = every ACTIVE location; Some = exactly these location ids.
    pub selected_locations: Option<Vec<String>>,
    /// Square-shaped JSON for `MockSquare::start`.
    pub mock: dsf_mock::Seed,
    /// The first check (first sync).
    pub first: Check,
    /// 0–4 later checks.
    pub later: Vec<Check>,
    /// Edge-case item id -> the case it covers, as `"<expect> — <what>"`.
    /// `<expect>` is `in@N` (not slow at check N: 0 = first), `out@N`
    /// (slow with a date), `date@N=<Mon D, YYYY>` (that date at check N),
    /// `months` (no sales in M+ months at the first check), `none` (no
    /// label at any check), `var1-none` (its second variation never has a
    /// label) or `label` (always labeled). These items are never touched by
    /// random sales, edits, puts or deletes; staff may still tamper with
    /// their labels. Use it to explain a failing seed.
    pub cases: BTreeMap<String, String>,
}

#[derive(Clone, Debug)]
pub struct Check {
    /// The app's clock and the mock's clock for this check.
    pub at: DateTime<Utc>,
    /// Applied to the mock just before this check (all empty for `first`).
    /// Appended orders; an existing id is an edited order with a newer
    /// `updated_at`.
    pub new_orders: Vec<Value>,
    /// New or replaced top-level ITEM objects.
    pub catalog_puts: Vec<Value>,
    /// Top-level item ids deleted.
    pub catalog_deletes: Vec<String>,
    /// Object id -> the value staff type by hand into this app's label
    /// (None = they remove it). The check must put the truth back.
    pub label_tampering: Vec<(String, Option<String>)>,
    /// Expected labels in Square after this check.
    pub truth: Truth,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Truth {
    /// Every live, non-archived item id -> expected label, or None for "no
    /// label from this app".
    pub items: BTreeMap<String, Option<String>>,
    /// Every live variation id of those items -> expected label or None.
    pub variations: BTreeMap<String, Option<String>>,
}

/// A small scenario, varied by seed.
pub fn scenario(seed: u64) -> Scenario {
    scenario_sized(seed, Size::small_for(seed))
}

/// A scenario of a given size (performance tests use 20,000 items and
/// 100,000 orders).
pub fn scenario_sized(seed: u64, size: Size) -> Scenario {
    build::build(seed, size)
}

impl Scenario {
    /// M: how many months the first sync reads (rule 1).
    pub fn history_months(&self) -> u32 {
        months_for_window(self.window_days)
    }

    /// The first check, then the later ones.
    pub fn checks(&self) -> impl Iterator<Item = &Check> {
        std::iter::once(&self.first).chain(self.later.iter())
    }

    /// Everything in the scenario as one JSON value (for comparing runs and
    /// for writing a failing seed to disk).
    pub fn to_json(&self) -> Value {
        let check = |c: &Check| {
            json!({
                "at": c.at.to_rfc3339_opts(SecondsFormat::Millis, true),
                "new_orders": c.new_orders,
                "catalog_puts": c.catalog_puts,
                "catalog_deletes": c.catalog_deletes,
                "label_tampering": c.label_tampering,
                "truth": { "items": c.truth.items, "variations": c.truth.variations },
            })
        };
        json!({
            "seed": self.seed,
            "window_days": self.window_days,
            "selected_locations": self.selected_locations,
            "mock": {
                "app_id": self.mock.app_id,
                "merchant": self.mock.merchant,
                "locations": self.mock.locations,
                "catalog": self.mock.catalog,
                "orders": self.mock.orders,
                "counts": self.mock.counts,
            },
            "first": check(&self.first),
            "later": self.later.iter().map(check).collect::<Vec<_>>(),
            "cases": self.cases,
        })
    }
}
