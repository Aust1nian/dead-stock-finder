//! Dedicated edge-case items, placed with exact dates. Random sales, edits,
//! puts and deletes never touch them (they are "pinned"), so each case
//! survives to the check it was built for.

use super::*;

/// The most items the recipes add (the rest of `Size::items` is random).
pub(super) const RECIPE_ITEMS: usize = 19;

impl Builder {
    fn pick_sel(&mut self) -> usize {
        let i = self.rng.index(self.sel.len());
        self.sel[i]
    }

    fn recipe_item(&mut self, presence: Presence, vars: Vec<Presence>) -> usize {
        let spec = ItemSpec { presence, vars, archived: false, weight: 0, pinned: true, seller_value: false };
        self.add_item(spec, None)
    }

    fn simple_item(&mut self, nvar: usize) -> usize {
        self.recipe_item(Presence::All { explicit: true }, vec![Presence::All { explicit: true }; nvar])
    }

    fn var_id(&self, item: usize, k: usize) -> String {
        self.items[item].vars[k].id.clone()
    }

    /// A sale `back` calendar days before check `x` in location `s`'s zone,
    /// 20-50 minutes later in the day than the check's local time (so whole
    /// elapsed days and calendar days disagree).
    fn boundary_time(&mut self, s: usize, x: usize, back: i64) -> DateTime<Utc> {
        let zone = self.locs[s].zone;
        let at = self.ats[x];
        let date = local_date(zone, at) - Duration::days(back);
        let now = i64::from(at.with_timezone(&zone).num_seconds_from_midnight());
        let mut secs = now + self.rng.range(20 * 60, 50 * 60);
        if secs >= 86_400 - 120 {
            secs = self.rng.range(0, 86_400 - 180);
        }
        local_instant(zone, date, (secs / 3600) as u32, (secs / 60 % 60) as u32, (secs % 60).min(50) as u32)
    }

    /// Before the window at the first check (slow), inside the history.
    fn old_time(&mut self) -> DateTime<Utc> {
        let back = self.days() + self.rng.range(5, 25);
        self.t1() - Duration::days(back) - Duration::seconds(self.rng.range(0, 80_000))
    }

    /// Well inside the window at the first check.
    fn recent_time(&mut self) -> DateTime<Utc> {
        let back = self.rng.range(1, (self.days() / 2).max(2));
        self.t1() - Duration::days(back) - Duration::seconds(self.rng.range(0, 80_000))
    }

    pub(super) fn recipes(&mut self) {
        if self.size.items < 40 {
            return;
        }
        let w = self.days();

        // Exactly `window` days (not slow) and `window + 1` (slow) before a
        // check; the second item has an older sale on another variation.
        for extra in [0, 1] {
            let x = self.rng.index(self.ats.len());
            let s = self.pick_sel();
            let it = self.simple_item(1 + extra as usize);
            let t = self.boundary_time(s, x, w + extra);
            let v = self.var_id(it, 0);
            if self.sale(&v, s, t, "1").is_some() {
                let (expect, days) = if extra == 0 { ("in", "window") } else { ("out", "window + 1") };
                let zone = self.locs[s].zone_name;
                self.note(it, format!("{expect}@{x} \u{2014} sold {days} days before check {x} at {zone}, later in the day than the check"));
            }
            if extra == 1 {
                let v1 = self.var_id(it, 1);
                let older = t - Duration::days(self.rng.range(3, 20));
                self.sale(&v1, s, older, "1");
            }
        }

        // Late night: the local date differs from the UTC date, on the
        // boundary day, at the westernmost selected location.
        {
            let x = self.rng.index(self.ats.len());
            let at = self.ats[x];
            let s = *self.sel.iter().min_by_key(|s| offset_secs(self.locs[**s].zone, at)).expect("selected");
            let zone = self.locs[s].zone;
            let off = offset_secs(zone, at);
            let (back, h, m) = if off < 0 {
                (w + 1, 23, self.rng.range(20, 50))
            } else if off > 0 {
                (w, 0, self.rng.range(5, 40))
            } else {
                (w + 1, 23, 30)
            };
            let date = local_date(zone, at) - Duration::days(back);
            let t = local_instant(zone, date, h, m as u32, self.rng.range(0, 50) as u32);
            let it = self.simple_item(1);
            let v = self.var_id(it, 0);
            if self.sale(&v, s, t, "1").is_some() {
                let expect = if back == w { "in" } else { "out" };
                let zone = self.locs[s].zone_name;
                self.note(
                    it,
                    format!(
                        "{expect}@{x} \u{2014} late-night sale at {zone}: local date differs from UTC date"
                    ),
                );
            }
        }

        // A daylight-saving change between a boundary sale and the check.
        let observes_dst = |zone: Tz| {
            let jan = Utc.with_ymd_and_hms(2026, 1, 15, 12, 0, 0).single().expect("date");
            let jul = Utc.with_ymd_and_hms(2026, 7, 15, 12, 0, 0).single().expect("date");
            offset_secs(zone, jan) != offset_secs(zone, jul)
        };
        let dst: Vec<usize> = self.sel.iter().copied().filter(|s| observes_dst(self.locs[*s].zone)).collect();
        if !dst.is_empty() {
            let s = *self.rng.pick(&dst);
            let zone = self.locs[s].zone;
            let found = (0..self.ats.len()).find(|x| {
                let at = self.ats[*x];
                offset_secs(zone, at) != offset_secs(zone, at - Duration::days(w + 1))
            });
            if let Some(x) = found {
                let back = w + self.rng.range(0, 1);
                let t = self.boundary_time(s, x, back);
                let it = self.simple_item(1);
                let v = self.var_id(it, 0);
                if self.sale(&v, s, t, "1").is_some() {
                    let expect = if back == w { "in" } else { "out" };
                    let zone = self.locs[s].zone_name;
                    self.note(it, format!("{expect}@{x} \u{2014} daylight-saving change at {zone} between the sale and check {x}"));
                }
            }
        }

        // Two selected zones: the later sale (by instant) is at the western
        // location but on an earlier local date than the eastern one.
        if self.sel.len() >= 2 {
            let x = self.rng.index(self.ats.len());
            let at = self.ats[x];
            let west =
                *self.sel.iter().min_by_key(|s| offset_secs(self.locs[**s].zone, at)).expect("selected");
            let east =
                *self.sel.iter().max_by_key(|s| offset_secs(self.locs[**s].zone, at)).expect("selected");
            let (zw, ze) = (self.locs[west].zone, self.locs[east].zone);
            if offset_secs(zw, at) != offset_secs(ze, at) {
                let day = local_date(zw, at) - Duration::days(w + self.rng.range(0, 1));
                let base = local_instant(zw, day, 12, 0, 0) - Duration::hours(36);
                let half = Duration::minutes(30);
                let hits: Vec<DateTime<Utc>> = (0..72 * 4)
                    .map(|k| base + Duration::minutes(15 * k) + Duration::seconds(7))
                    .filter(|t| {
                        local_date(zw, *t + half) == day && local_date(ze, *t) == day + Duration::days(1)
                    })
                    .collect();
                if !hits.is_empty() {
                    let t = *self.rng.pick(&hits);
                    let it = self.simple_item(1);
                    let v = self.var_id(it, 0);
                    let a = self.sale(&v, east, t, "1");
                    let b = self.sale(&v, west, t + half, "1");
                    if a.is_some() && b.is_some() {
                        let (we, ee) = (self.locs[west].zone_name, self.locs[east].zone_name);
                        let date = crate::truth::date_text(day);
                        self.note(it, format!("date@{x}={date} \u{2014} later sale at western {we} has an earlier local date than {ee}"));
                    }
                }
            }
        }

        // Never sold; sold only before the history start.
        let n = self.rng.range(1, 3) as usize;
        let it = self.simple_item(n);
        self.note(it, "months \u{2014} never sold".to_string());
        {
            let s = self.pick_sel();
            let it = self.simple_item(1);
            let t = self.hs
                - Duration::days(self.rng.range(10, 60))
                - Duration::seconds(self.rng.range(0, 80_000));
            let v = self.var_id(it, 0);
            self.sale(&v, s, t, "1");
            self.note(it, "months \u{2014} sold only before the history start".to_string());
        }

        // A recent sale at an unselected (or inactive) location over an old
        // one at a selected location: slow.
        if !self.unsel.is_empty() {
            let s = self.pick_sel();
            let u = *self.rng.pick(&self.unsel.clone());
            let it = self.simple_item(1);
            let v = self.var_id(it, 0);
            let (old, recent) = (self.old_time(), self.recent_time());
            self.sale(&v, s, old, "1");
            self.sale(&v, u, recent, "1");
            let zone = self.locs[u].zone_name;
            self.note(it, format!("out@0 \u{2014} recent sale only at unselected {zone}"));
        }

        // Recent canceled and open orders with catalog lines: slow.
        {
            let s = self.pick_sel();
            let it = self.simple_item(1);
            let v = self.var_id(it, 0);
            let old = self.old_time();
            self.sale(&v, s, old, "1");
            let line = json!({ "catalog_object_id": v, "quantity": "1", "item_type": "ITEM" });
            let (t1, t2) = (self.recent_time(), self.recent_time());
            self.place(s, State::Canceled, t1, vec![line.clone()], None);
            self.place(s, State::Open, t2, vec![line], None);
            self.note(it, "out@0 \u{2014} recent canceled and open orders".to_string());
        }

        // Quantities "0" and "-1" never count; "0.25" does.
        {
            let s = self.pick_sel();
            let it = self.simple_item(1);
            let v = self.var_id(it, 0);
            let old = self.old_time();
            self.sale(&v, s, old, "1");
            let lines = vec![
                json!({ "catalog_object_id": v, "quantity": "0", "item_type": "ITEM" }),
                json!({ "catalog_object_id": v, "quantity": "-1", "item_type": "ITEM" }),
            ];
            let t = self.recent_time();
            self.place(s, State::Completed, t, lines, None);
            self.note(it, "out@0 \u{2014} recent quantities 0 and -1".to_string());

            let it = self.simple_item(1);
            let v = self.var_id(it, 0);
            let old = self.old_time();
            self.sale(&v, s, old, "1");
            let t = self.recent_time();
            self.sale(&v, s, t, "0.25");
            self.note(it, "in@0 \u{2014} recent quantity 0.25".to_string());
        }

        // A custom-amount line naming a catalog variation (half the seeds),
        // beside a gift-card line: neither counts.
        {
            let s = self.pick_sel();
            let it = self.simple_item(1);
            let v = self.var_id(it, 0);
            let old = self.old_time();
            self.sale(&v, s, old, "1");
            let custom = if self.rng.chance(50) {
                json!({ "catalog_object_id": v, "name": "Custom amount", "quantity": "1", "item_type": "CUSTOM_AMOUNT" })
            } else {
                Self::custom_line()
            };
            let t = self.recent_time();
            self.place(s, State::Completed, t, vec![custom, Self::gift_card_line()], None);
            self.note(it, "out@0 \u{2014} recent custom-amount and gift-card lines".to_string());
        }

        // A return after the last sale; an exchange that returns one item and
        // sells another.
        {
            let s = self.pick_sel();
            let it = self.simple_item(1);
            let v = self.var_id(it, 0);
            let old = self.old_time();
            if let Some(source) = self.sale(&v, s, old, "1") {
                let back = json!({ "catalog_object_id": v, "quantity": "1", "item_type": "ITEM" });
                let t = self.recent_time();
                self.place(s, State::Completed, t, Vec::new(), Some((source, vec![back])));
                self.note(it, "out@0 \u{2014} recent return order".to_string());
            }
            let returned = self.simple_item(1);
            let sold = self.simple_item(1);
            let (vr, vs) = (self.var_id(returned, 0), self.var_id(sold, 0));
            let old = self.old_time();
            if let Some(source) = self.sale(&vr, s, old, "1") {
                let back = json!({ "catalog_object_id": vr, "quantity": "1", "item_type": "ITEM" });
                let line = json!({ "catalog_object_id": vs, "quantity": "1", "item_type": "ITEM" });
                let t = self.recent_time();
                self.place(s, State::Completed, t, vec![line], Some((source, vec![back])));
                self.note(returned, "out@0 \u{2014} returned in a recent exchange".to_string());
                self.note(sold, "in@0 \u{2014} sold in a recent exchange".to_string());
            }
        }

        // Carried only at unselected locations (no labels, despite sales);
        // carried at only some locations.
        if !self.unsel.is_empty() {
            let u = *self.rng.pick(&self.unsel.clone());
            let it = self.recipe_item(Presence::Only(vec![u]), vec![Presence::All { explicit: true }]);
            let v = self.var_id(it, 0);
            let t = self.recent_time();
            self.sale(&v, u, t, "1");
            self.note(it, "none \u{2014} carried only at an unselected location".to_string());
        }
        if self.locs.len() >= 2 {
            let s = self.pick_sel();
            let mut only = vec![s];
            if let Some(u) = self.unsel.first() {
                only.push(*u);
            }
            only.sort_unstable();
            let it = self.recipe_item(Presence::Only(only), vec![Presence::All { explicit: true }]);
            self.note(it, "label \u{2014} carried at only some locations".to_string());
            let v = self.var_id(it, 0);
            let t = self.t1()
                - Duration::days(self.rng.range(1, 2 * w))
                - Duration::seconds(self.rng.range(0, 80_000));
            self.sale(&v, s, t, "1");
        }

        // A variation absent at every selected location (no label), and one
        // absent at just one of several selected locations.
        {
            let mut vars = vec![Presence::All { explicit: true }, Presence::Except(self.sel.clone())];
            if self.sel.len() >= 2 {
                vars.push(Presence::Except(vec![self.sel[0]]));
            }
            let it = self.recipe_item(Presence::All { explicit: true }, vars);
            self.note(
                it,
                "var1-none \u{2014} second variation absent at every selected location".to_string(),
            );
            let s = self.pick_sel();
            let v0 = self.var_id(it, 0);
            let t = self.t1()
                - Duration::days(self.rng.range(1, 2 * w))
                - Duration::seconds(self.rng.range(0, 80_000));
            self.sale(&v0, s, t, "1");
            if let Some(&u) = self.items[it].vars[1].carried.first() {
                let v1 = self.var_id(it, 1);
                let t = self.recent_time();
                self.sale(&v1, u, t, "1");
            }
            if self.sel.len() >= 2 {
                let v2 = self.var_id(it, 2);
                let s1 = self.sel[1];
                let t = self.old_time();
                self.sale(&v2, s1, t, "1");
            }
        }

        // About 30% of seeds: an item with 50 variations.
        if self.rng.chance(30) {
            let n = self.locs.len();
            let vars = (0..50)
                .map(|_| {
                    if self.rng.chance(10) {
                        Presence::Except(vec![self.rng.index(n)])
                    } else {
                        Presence::All { explicit: true }
                    }
                })
                .collect();
            let it = self.recipe_item(Presence::All { explicit: true }, vars);
            self.note(it, "label \u{2014} 50 variations".to_string());
            let lo = self.hs + Duration::days(5);
            let span = (self.t1() - Duration::hours(1) - lo).num_seconds();
            for _ in 0..12 {
                let k = self.rng.index(50);
                let carried = self.items[it].vars[k].carried.clone();
                if carried.is_empty() {
                    continue;
                }
                let l = *self.rng.pick(&carried);
                let v = self.var_id(it, k);
                let t = lo + Duration::seconds(self.rng.range(0, span));
                self.sale(&v, l, t, "1");
            }
        }
    }
}
