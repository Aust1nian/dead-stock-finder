//! What changes in Square between checks: new and edited orders, catalog
//! puts and deletes, hand-edited labels.

use super::*;

impl Builder {
    fn random_in(&mut self, from: DateTime<Utc>, at: DateTime<Utc>) -> DateTime<Utc> {
        let span = (at - from).num_milliseconds();
        from + Duration::milliseconds(self.rng.range(2_000, span - 2_000))
    }

    /// A random live, unpinned item not yet changed in this check.
    fn pick_existing(&mut self, touched: &HashSet<usize>, live_only: bool) -> Option<usize> {
        if self.items.is_empty() {
            return None;
        }
        for _ in 0..200 {
            let i = self.rng.index(self.items.len());
            let it = &self.items[i];
            if !it.pinned && !it.deleted && (!live_only || !it.archived) && !touched.contains(&i) {
                return Some(i);
            }
        }
        None
    }

    fn loc_index(&self, id: &str) -> Option<usize> {
        self.locs.iter().position(|l| l.id == id)
    }

    /// The item as staff save it: the current catalog object with the
    /// labels Square held after the previous check (seller values kept).
    fn put_of(&self, item: usize, previous: &Truth) -> Value {
        let id = &self.items[item].id;
        let mut obj = self.world.items[id].clone();
        set_label(&mut obj, previous.items.get(id).cloned().flatten().as_deref());
        if let Some(vars) = obj.pointer_mut("/item_data/variations").and_then(Value::as_array_mut) {
            for v in vars.iter_mut() {
                let vid = v["id"].as_str().unwrap_or_default().to_string();
                set_label(v, previous.variations.get(&vid).cloned().flatten().as_deref());
            }
        }
        obj
    }

    fn bump_version(order: &mut Value) {
        if let Some(v) = order.get("version").and_then(Value::as_i64) {
            order["version"] = json!(v + 1);
        }
    }

    pub(super) fn later_check(&mut self, k: usize, previous: &Truth) -> Check {
        let (from, at) = (self.ats[k - 1], self.ats[k]);
        let span_s = (at - from).num_seconds();
        let mut touched: HashSet<usize> = HashSet::new();
        let mut puts: Vec<Value> = Vec::new();
        let mut deletes: Vec<String> = Vec::new();

        // New sales at the usual rate.
        let history_s = ((self.t1() - self.hs).num_days() + 60) * 86_400;
        let n = (self.size.orders as i64 * span_s / history_s).clamp(2, self.size.orders as i64 / 2 + 2);
        for _ in 0..n {
            let t = self.random_in(from, at);
            let Some(item) = self.pick_weighted() else { break };
            let Some((var, loc)) = self.sellable(item, None) else { continue };
            let state = match self.rng.below(100) {
                0..94 => State::Completed,
                94..97 => State::Canceled,
                _ => State::Open,
            };
            let completed = matches!(state, State::Completed);
            let qty = self.quantity();
            let mut lines = vec![self.line(&var, qty)];
            if self.rng.chance(5) {
                lines.push(Self::custom_line());
            }
            if let Some(id) = self.place(loc, state, t, lines, None)
                && completed
            {
                self.editable.insert(id);
            }
        }

        // Slow items sell again at a selected location.
        let slow: Vec<usize> = previous
            .items
            .iter()
            .filter(|(_, label)| label.as_deref().is_some_and(|l| l.starts_with("SLOW")))
            .filter_map(|(id, _)| self.item_index.get(id).copied())
            .filter(|i| !self.items[*i].pinned && self.items[*i].live())
            .collect();
        for _ in 0..slow.len().min(3) {
            let i = *self.rng.pick(&slow);
            let s = self.sel[self.rng.index(self.sel.len())];
            if let Some((var, loc)) = self.sellable(i, Some(s)) {
                let t = self.random_in(from, at);
                if let Some(id) = self.sale(&var, loc, t, "1") {
                    self.editable.insert(id);
                }
            }
        }

        // Orders edited after the checkpoint: re-sent with a newer
        // updated_at and a line removed, zeroed or switched.
        let floor = self.hs + Duration::days(3);
        let mut candidates: Vec<(DateTime<Utc>, String)> = self
            .world
            .orders
            .iter()
            .filter(|(id, o)| self.editable.contains(*id) && o["state"] == "COMPLETED")
            .filter_map(|(id, o)| crate::truth::parse_time(&o["closed_at"]).map(|t| (t, id.clone())))
            .filter(|(t, _)| *t >= floor)
            .collect();
        candidates.sort();
        candidates.reverse();
        candidates.truncate(40);
        let mut edited: HashSet<String> = HashSet::new();
        for _ in 0..self.rng.range(1, 3) {
            if candidates.is_empty() {
                break;
            }
            let id = candidates[self.rng.index(candidates.len())].1.clone();
            if !edited.insert(id.clone()) {
                continue;
            }
            let mut order = self.world.orders[&id].clone();
            let loc = order["location_id"].as_str().and_then(|l| self.loc_index(l)).unwrap_or(0);
            let catalog_lines: Vec<usize> = order["line_items"]
                .as_array()
                .map(|lines| {
                    (0..lines.len()).filter(|i| lines[*i].get("catalog_object_id").is_some()).collect()
                })
                .unwrap_or_default();
            if catalog_lines.is_empty() {
                continue;
            }
            let li = *self.rng.pick(&catalog_lines);
            let switch_to = match self.rng.below(3) {
                0 => None,
                1 => Some(None),
                _ => Some(self.pick_weighted().and_then(|i| self.sellable(i, Some(loc))).map(|(v, _)| v)),
            };
            let lines = order["line_items"].as_array_mut().expect("lines");
            match switch_to {
                None => {
                    lines.remove(li);
                }
                Some(None) => lines[li]["quantity"] = json!("0"),
                Some(Some(v)) => lines[li]["catalog_object_id"] = json!(v),
            }
            let updated = self.random_in(from, at);
            order["updated_at"] = json!(ts(updated));
            Self::bump_version(&mut order);
            self.buckets[k].push(order);
        }

        // An open order from before is now completed.
        if !self.open_orders.is_empty() && self.rng.chance(60) {
            let i = self.rng.index(self.open_orders.len());
            let mut order = self.open_orders.remove(i);
            let t = self.random_in(from, at);
            let t = self.unique(t);
            let updated = self.updated_for(k, t);
            order["state"] = json!("COMPLETED");
            order["updated_at"] = json!(ts(updated));
            order["closed_at"] = json!(ts(t));
            Self::bump_version(&mut order);
            if let Some(id) = order["id"].as_str() {
                self.editable.insert(id.to_string());
            }
            self.buckets[k].push(order);
        }

        // An order closed before the previous check arrives late.
        if self.rng.chance(60)
            && let Some(item) = self.pick_weighted()
            && let Some((var, loc)) = self.sellable(item, None)
        {
            let t = from - Duration::seconds(self.rng.range(3_600, 20 * 3_600));
            let t = self.unique(t);
            let updated = self.random_in(from, at);
            let id = self.new_order_id();
            let line = self.line(&var, "1");
            let order = self.order_json(&id, loc, &State::Completed, t, updated, vec![line], None);
            self.buckets[k].push(order);
            self.editable.insert(id);
        }

        // New items: some sold, some "no sales since added".
        for _ in 0..self.rng.range(1, 3) {
            let mut spec = self.regular_spec(false);
            spec.vars.truncate(3);
            let i = self.add_item(spec, Some(at));
            touched.insert(i);
            puts.push(self.put_of(i, previous));
            if self.rng.chance(50)
                && let Some((v, l)) = self.sellable(i, None)
            {
                let t = self.random_in(from, at);
                self.sale(&v, l, t, "1");
            }
        }

        // A new variation on an existing item.
        if self.rng.chance(60)
            && let Some(i) = self.pick_existing(&touched, true)
        {
            let item_id = self.items[i].id.clone();
            let n = self.items[i].vars.len();
            let var_id = format!("V{:06}-{:02}", self.items[i].num, n + 1);
            let v = self.variation_json(&item_id, &var_id, n, 2, &Presence::All { explicit: true });
            let item = self.world.items.get_mut(&item_id).expect("live item");
            if let Some(vars) = item.pointer_mut("/item_data/variations").and_then(Value::as_array_mut) {
                vars.push(v.clone());
            }
            let carried = self.carried(&self.world.items[&item_id], &v);
            self.items[i].vars.push(VarModel { id: var_id.clone(), carried, removed: false });
            self.world.first_seen.insert(var_id.clone(), at);
            touched.insert(i);
            puts.push(self.put_of(i, previous));
            if self.rng.chance(50)
                && let Some(&l) = self.items[i].vars[n].carried.first()
            {
                let t = self.random_in(from, at);
                self.sale(&var_id, l, t, "1");
            }
        }

        // A variation removed (never the first, so the item keeps one it
        // had when first seen).
        if self.rng.chance(50) {
            for _ in 0..20 {
                let Some(i) = self.pick_existing(&touched, true) else { break };
                let live: Vec<usize> =
                    (1..self.items[i].vars.len()).filter(|k| !self.items[i].vars[*k].removed).collect();
                if live.is_empty() {
                    continue;
                }
                let kk = *self.rng.pick(&live);
                let var_id = self.items[i].vars[kk].id.clone();
                self.items[i].vars[kk].removed = true;
                let item_id = self.items[i].id.clone();
                let item = self.world.items.get_mut(&item_id).expect("live item");
                if let Some(vars) = item.pointer_mut("/item_data/variations").and_then(Value::as_array_mut) {
                    vars.retain(|v| v["id"] != var_id.as_str());
                }
                touched.insert(i);
                puts.push(self.put_of(i, previous));
                break;
            }
        }

        // A rename (no label change) and, sometimes, an archive.
        if self.rng.chance(50)
            && let Some(i) = self.pick_existing(&touched, true)
        {
            let item = self.world.items.get_mut(&self.items[i].id).expect("live item");
            if let Some(name) = item.pointer_mut("/item_data/name") {
                *name = json!(format!("{} (edited)", name.as_str().unwrap_or("")));
            }
            touched.insert(i);
            puts.push(self.put_of(i, previous));
        }
        if self.rng.chance(25)
            && let Some(i) = self.pick_existing(&touched, true)
        {
            let item = self.world.items.get_mut(&self.items[i].id).expect("live item");
            if let Some(data) = item.get_mut("item_data").and_then(Value::as_object_mut) {
                data.insert("is_archived".into(), json!(true));
            }
            self.items[i].archived = true;
            touched.insert(i);
            puts.push(self.put_of(i, previous));
        }

        // Deletes.
        for _ in 0..self.rng.range(0, 2) {
            if let Some(i) = self.pick_existing(&touched, false) {
                self.items[i].deleted = true;
                self.world.items.remove(&self.items[i].id);
                deletes.push(self.items[i].id.clone());
                touched.insert(i);
            }
        }

        // Staff edit or remove labels by hand; sometimes type one where this
        // app has none.
        let mut labeled = Vec::new();
        let mut unlabeled = Vec::new();
        for (i, it) in self.items.iter().enumerate() {
            if !it.live() || touched.contains(&i) {
                continue;
            }
            let ids = std::iter::once((&it.id, previous.items.get(&it.id))).chain(
                it.vars.iter().filter(|v| !v.removed).map(|v| (&v.id, previous.variations.get(&v.id))),
            );
            for (id, label) in ids {
                match label {
                    Some(Some(text)) => labeled.push((id.clone(), text.clone())),
                    Some(None) => unlabeled.push(id.clone()),
                    None => {}
                }
            }
        }
        let mut tampering: Vec<(String, Option<String>)> = Vec::new();
        for _ in 0..self.rng.range(1, 3) {
            if labeled.is_empty() {
                break;
            }
            let (id, text) = labeled[self.rng.index(labeled.len())].clone();
            if tampering.iter().any(|(t, _)| *t == id) {
                continue;
            }
            let value = match self.rng.below(3) {
                0 => None,
                1 => Some("Last sold Jan 1, 2020".to_string()),
                _ => Some(format!("{text} (checked)")),
            };
            tampering.push((id, value));
        }
        if self.rng.chance(25) && !unlabeled.is_empty() {
            let id = unlabeled[self.rng.index(unlabeled.len())].clone();
            tampering.push((id, Some("Last sold Jul 4, 2024".to_string())));
        }

        let new_orders = self.buckets[k].clone();
        for o in &new_orders {
            self.world.learn_order(o.clone());
        }
        let truth = self.world.truth(at);
        Check {
            at,
            new_orders,
            catalog_puts: puts,
            catalog_deletes: deletes,
            label_tampering: tampering,
            truth,
        }
    }
}
