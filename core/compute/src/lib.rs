//! Last-sold dates, slow flags and label text.
//!
//! Everything here is a pure function of what the cache holds and the clock
//! the caller passes in. The generator computes the same truth on its own and
//! the property tests compare the two, so this file is the rulebook:
//!
//! - A sale counts at the location where it was rung up, on that location's
//!   calendar date. A variation's last sale is its latest sale at the selected
//!   locations; an item's is the latest across its variations.
//! - Slow means the last sale is older than the window: more than
//!   `window_days` days before today at the selling location.
//! - No sale anywhere in the history read gives "no sales in N+ months", never
//!   a made-up date. An item added since the history began gives "no sales
//!   since added", and is slow only once it has existed longer than the window.
//! - An item or variation carried at none of the selected locations gets no
//!   label at all.

use chrono::{DateTime, Datelike, Months, NaiveDate, Utc};

/// The key of the app's one catalog custom attribute. Never changed once
/// sellers have labels: a new key would orphan every value written.
pub const LABEL_KEY: &str = "dead_stock_last_sold";
/// The attribute's name in Square's screens.
pub const LABEL_NAME: &str = "Last sold";
pub const LABEL_DESCRIPTION: &str = "Updated daily by Dead Stock Finder.";
use chrono_tz::Tz;
use serde::Serialize;

/// Which locations carry a catalog object, in Square's terms.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Presence {
    /// `present_at_all_locations`; Square's default when absent is true.
    pub all: bool,
    pub present_at: Vec<String>,
    pub absent_at: Vec<String>,
}

impl Default for Presence {
    fn default() -> Self {
        Presence { all: true, present_at: Vec::new(), absent_at: Vec::new() }
    }
}

impl Presence {
    pub fn carried_at(&self, location_id: &str) -> bool {
        if self.all {
            !self.absent_at.iter().any(|l| l == location_id)
        } else {
            self.present_at.iter().any(|l| l == location_id)
        }
    }
}

#[derive(Clone, Debug)]
pub struct Location {
    pub id: String,
    pub tz: Tz,
}

/// One sale instant at one location. Callers pass only the latest sale per
/// location, but any number works.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sale {
    pub location_id: String,
    pub at: DateTime<Utc>,
}

/// What the app knows about when an item or variation came to exist.
#[derive(Clone, Debug)]
pub struct Origin {
    /// Square's own creation time, when the catalog carries one.
    pub created_at: Option<DateTime<Utc>>,
    /// When this app first saw the object in the catalog.
    pub first_seen: DateTime<Utc>,
}

/// How far back the order history goes.
#[derive(Clone, Debug)]
pub struct History {
    /// The earliest instant the order read covers.
    pub start: DateTime<Utc>,
    /// The months in "no sales in N+ months": `start` is exactly this many
    /// calendar months before the read that set it, so the wording stays true.
    pub months: u32,
    /// When the first sync read the catalog. Without a Square creation date,
    /// anything first seen after this was added since.
    pub first_sync_at: DateTime<Utc>,
}

#[derive(Clone, Debug)]
pub struct Rules {
    pub window_days: u32,
    /// The selected locations. Sales elsewhere don't count, and an object
    /// carried at none of these gets no label.
    pub locations: Vec<Location>,
    /// The time zone for dates that belong to no location ("added").
    pub home_tz: Tz,
    pub now: DateTime<Utc>,
    pub history: History,
}

impl Rules {
    fn location(&self, id: &str) -> Option<&Location> {
        self.locations.iter().find(|l| l.id == id)
    }

    fn today_in(&self, tz: Tz) -> NaiveDate {
        self.now.with_timezone(&tz).date_naive()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LastSold {
    Sold {
        /// The calendar date at the selling location.
        date: NaiveDate,
        at: DateTime<Utc>,
        location_id: String,
        days: i64,
    },
    NoSalesSinceAdded {
        date: NaiveDate,
        days: i64,
    },
    NoSalesInHistory {
        months: u32,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Verdict {
    pub last: LastSold,
    pub slow: bool,
}

impl Verdict {
    /// The text written into Square.
    pub fn label(&self) -> String {
        match (&self.last, self.slow) {
            (LastSold::Sold { date, .. }, false) => format!("Last sold {}", show_date(*date)),
            (LastSold::Sold { date, .. }, true) => format!("SLOW · last sold {}", show_date(*date)),
            (LastSold::NoSalesSinceAdded { date, .. }, false) => {
                format!("No sales since added {}", show_date(*date))
            }
            (LastSold::NoSalesSinceAdded { date, .. }, true) => {
                format!("SLOW · no sales since added {}", show_date(*date))
            }
            (LastSold::NoSalesInHistory { months }, _) => format!("SLOW · no sales in {months}+ months"),
        }
    }

    /// Days since the last sale, or since it was added; none when no sale
    /// was found in the history read.
    pub fn days(&self) -> Option<i64> {
        match &self.last {
            LastSold::Sold { days, .. } | LastSold::NoSalesSinceAdded { days, .. } => Some(*days),
            LastSold::NoSalesInHistory { .. } => None,
        }
    }
}

/// "Jul 14, 2026". English month abbreviations whatever the computer's
/// language, because the label is the same text for every reader in Square.
pub fn show_date(date: NaiveDate) -> String {
    const MONTHS: [&str; 12] =
        ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    format!("{} {}, {}", MONTHS[date.month0() as usize], date.day(), date.year())
}

/// The verdict for one item or variation, or `None` when no selected location
/// carries it.
///
/// `presence` is the object's effective presence (for a variation, also
/// within its item's). `sales` are its own sales, or for an item, all its
/// variations' sales; sales at unselected locations are ignored here.
pub fn verdict<'a>(
    presence: &Presence,
    origin: &Origin,
    sales: impl IntoIterator<Item = &'a Sale>,
    rules: &Rules,
) -> Option<Verdict> {
    if !rules.locations.iter().any(|l| presence.carried_at(&l.id)) {
        return None;
    }
    let latest = sales
        .into_iter()
        .filter_map(|s| rules.location(&s.location_id).map(|l| (s, l)))
        .max_by(|(a, _), (b, _)| a.at.cmp(&b.at).then_with(|| b.location_id.cmp(&a.location_id)));
    if let Some((sale, location)) = latest {
        let date = sale.at.with_timezone(&location.tz).date_naive();
        let days = (rules.today_in(location.tz) - date).num_days().max(0);
        return Some(Verdict {
            last: LastSold::Sold { date, at: sale.at, location_id: sale.location_id.clone(), days },
            slow: days > i64::from(rules.window_days),
        });
    }
    // No sale at the selected locations anywhere in the history read.
    let added = match origin.created_at {
        Some(created) => (created > rules.history.start).then_some(created),
        None => (origin.first_seen > rules.history.first_sync_at).then_some(origin.first_seen),
    };
    Some(match added {
        Some(added) => {
            let date = added.with_timezone(&rules.home_tz).date_naive();
            let days = (rules.today_in(rules.home_tz) - date).num_days().max(0);
            Verdict {
                last: LastSold::NoSalesSinceAdded { date, days },
                slow: days > i64::from(rules.window_days),
            }
        }
        None => Verdict { last: LastSold::NoSalesInHistory { months: rules.history.months }, slow: true },
    })
}

/// How many calendar months the history must cover: 12, or the window plus
/// 30 days if that is longer, rounded up to whole months so "N+ months" holds.
pub fn history_months(window_days: u32) -> u32 {
    let needed_days = i64::from(window_days) + 30;
    let mut months = 12u32;
    // A calendar month is at least 28 days, so this terminates quickly; use
    // the shortest stretch of N months (starting in February of a common
    // year) to be sure every N-month span covers the days needed.
    while shortest_span_days(months) < needed_days {
        months += 1;
    }
    months
}

fn shortest_span_days(months: u32) -> i64 {
    let mut shortest = i64::MAX;
    for start_month in 1..=12 {
        for year in [2025, 2027, 2028] {
            let start = NaiveDate::from_ymd_opt(year, start_month, 1).unwrap();
            let end = start + Months::new(months);
            shortest = shortest.min((end - start).num_days());
        }
    }
    shortest
}

/// The start of a history read made at `now` covering `months` months.
pub fn history_start(now: DateTime<Utc>, months: u32) -> DateTime<Utc> {
    now.checked_sub_months(Months::new(months)).expect("date in range")
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn utc(y: i32, m: u32, d: u32, h: u32, min: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, m, d, h, min, 0).unwrap()
    }

    fn rules(now: DateTime<Utc>) -> Rules {
        Rules {
            window_days: 90,
            locations: vec![
                Location { id: "L1".into(), tz: chrono_tz::America::New_York },
                Location { id: "L2".into(), tz: chrono_tz::America::Los_Angeles },
            ],
            home_tz: chrono_tz::America::New_York,
            now,
            history: History {
                start: utc(2025, 10, 12, 9, 0),
                months: 12,
                first_sync_at: utc(2026, 10, 12, 9, 0),
            },
        }
    }

    fn origin_old() -> Origin {
        Origin { created_at: None, first_seen: utc(2026, 10, 12, 9, 0) }
    }

    fn sale(loc: &str, at: DateTime<Utc>) -> Sale {
        Sale { location_id: loc.into(), at }
    }

    #[test]
    fn date_format() {
        assert_eq!(show_date(NaiveDate::from_ymd_opt(2026, 7, 14).unwrap()), "Jul 14, 2026");
        assert_eq!(show_date(NaiveDate::from_ymd_opt(2026, 8, 2).unwrap()), "Aug 2, 2026");
    }

    #[test]
    fn recent_sale_inside_window() {
        let r = rules(utc(2026, 10, 12, 12, 0));
        let v =
            verdict(&Presence::default(), &origin_old(), &[sale("L1", utc(2026, 7, 14, 18, 0))], &r).unwrap();
        assert_eq!(v.label(), "Last sold Jul 14, 2026");
        assert_eq!(v.days(), Some(90));
        assert!(!v.slow, "exactly the window is not older than it");
    }

    #[test]
    fn sale_past_window_is_slow() {
        let r = rules(utc(2026, 10, 12, 12, 0));
        let v =
            verdict(&Presence::default(), &origin_old(), &[sale("L1", utc(2026, 7, 13, 18, 0))], &r).unwrap();
        assert_eq!(v.label(), "SLOW · last sold Jul 13, 2026");
        assert_eq!(v.days(), Some(91));
    }

    #[test]
    fn local_date_at_the_selling_location() {
        // 02:30 UTC on Jul 15 is still Jul 14 in New York.
        let r = rules(utc(2026, 10, 12, 12, 0));
        let v =
            verdict(&Presence::default(), &origin_old(), &[sale("L1", utc(2026, 7, 15, 2, 30))], &r).unwrap();
        assert_eq!(v.label(), "Last sold Jul 14, 2026");
    }

    #[test]
    fn latest_across_locations_and_unselected_ignored() {
        let mut r = rules(utc(2026, 10, 12, 12, 0));
        r.locations.truncate(1);
        let sales = [sale("L1", utc(2026, 5, 1, 15, 0)), sale("L2", utc(2026, 9, 1, 15, 0))];
        let v = verdict(&Presence::default(), &origin_old(), &sales, &r).unwrap();
        assert_eq!(v.label(), "SLOW · last sold May 1, 2026");
        let r = rules(utc(2026, 10, 12, 12, 0));
        let v = verdict(&Presence::default(), &origin_old(), &sales, &r).unwrap();
        assert_eq!(v.label(), "Last sold Sep 1, 2026");
    }

    #[test]
    fn no_sales_in_history() {
        let r = rules(utc(2026, 10, 12, 12, 0));
        let v = verdict(&Presence::default(), &origin_old(), &[], &r).unwrap();
        assert_eq!(v.label(), "SLOW · no sales in 12+ months");
        assert_eq!(v.days(), None);
    }

    #[test]
    fn added_after_first_sync() {
        let r = rules(utc(2026, 10, 12, 12, 0));
        let o = Origin { created_at: None, first_seen: utc(2026, 8, 2, 14, 0) };
        let mut r2 = r.clone();
        r2.history.first_sync_at = utc(2026, 6, 1, 0, 0);
        let v = verdict(&Presence::default(), &o, &[], &r2).unwrap();
        assert_eq!(v.label(), "No sales since added Aug 2, 2026");
        // Seen in the first sync itself: unknown age, so treated as old.
        let v = verdict(&Presence::default(), &origin_old(), &[], &r).unwrap();
        assert!(matches!(v.last, LastSold::NoSalesInHistory { .. }));
    }

    #[test]
    fn added_becomes_slow_after_window() {
        let mut r = rules(utc(2026, 12, 1, 12, 0));
        r.history.first_sync_at = utc(2026, 6, 1, 0, 0);
        let o = Origin { created_at: None, first_seen: utc(2026, 8, 2, 14, 0) };
        let v = verdict(&Presence::default(), &o, &[], &r).unwrap();
        assert_eq!(v.label(), "SLOW · no sales since added Aug 2, 2026");
        assert_eq!(v.days(), Some(121));
    }

    #[test]
    fn square_creation_date_wins_over_first_seen() {
        let r = rules(utc(2026, 10, 12, 12, 0));
        // Created inside the history read, seen first at the first sync.
        let o = Origin { created_at: Some(utc(2026, 9, 20, 16, 0)), first_seen: utc(2026, 10, 12, 9, 0) };
        let v = verdict(&Presence::default(), &o, &[], &r).unwrap();
        assert_eq!(v.label(), "No sales since added Sep 20, 2026");
        // Created before the history began.
        let o = Origin { created_at: Some(utc(2024, 1, 1, 16, 0)), first_seen: utc(2026, 10, 12, 9, 0) };
        let v = verdict(&Presence::default(), &o, &[], &r).unwrap();
        assert_eq!(v.label(), "SLOW · no sales in 12+ months");
    }

    #[test]
    fn not_carried_gets_no_label() {
        let r = rules(utc(2026, 10, 12, 12, 0));
        let p = Presence { all: false, present_at: vec!["L9".into()], absent_at: vec![] };
        assert!(verdict(&p, &origin_old(), &[], &r).is_none());
        let p = Presence { all: true, present_at: vec![], absent_at: vec!["L1".into(), "L2".into()] };
        assert!(verdict(&p, &origin_old(), &[], &r).is_none());
        let p = Presence { all: true, present_at: vec![], absent_at: vec!["L1".into()] };
        assert!(verdict(&p, &origin_old(), &[], &r).is_some());
    }

    #[test]
    fn daylight_saving_change_keeps_calendar_days() {
        // US clocks fall back on Nov 1, 2026. A sale late on Oct 31 local is
        // still 1 day before Nov 1 local, and the window counts dates.
        let mut r = rules(utc(2026, 11, 2, 5, 30)); // Nov 2 00:30 EST
        r.window_days = 1;
        let v =
            verdict(&Presence::default(), &origin_old(), &[sale("L1", utc(2026, 11, 1, 3, 59))], &r).unwrap();
        // 03:59 UTC Nov 1 = 23:59 EDT Oct 31.
        assert_eq!(v.label(), "SLOW · last sold Oct 31, 2026");
        assert_eq!(v.days(), Some(2));
    }

    #[test]
    fn months_cover_window_plus_30() {
        assert_eq!(history_months(30), 12);
        assert_eq!(history_months(90), 12);
        assert_eq!(history_months(335), 12);
        assert!(history_months(336) >= 13);
        for w in [400u32, 500, 1000] {
            let m = history_months(w);
            assert!(shortest_span_days(m) >= i64::from(w) + 30);
            assert!(shortest_span_days(m - 1) < i64::from(w) + 30);
        }
        assert_eq!(history_start(utc(2026, 10, 12, 9, 0), 12), utc(2025, 10, 12, 9, 0));
    }
}
