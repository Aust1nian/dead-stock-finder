//! The "Last sold" label in Square: its attribute definition and the
//! safe-write procedure every label write goes through, no exceptions.
//!
//! 1. Fetch the item fresh from Square.
//! 2. Copy it and change only this app's key in its custom attribute values
//!    (item level, and nested variations' where they carry labels).
//! 3. Prove the outgoing object differs from the fetched one only in that key
//!    ([`diff::only_label_changed`]); otherwise skip it and log why.
//! 4. Send it with its version. A version conflict starts again from 1, up to
//!    3 times, then the item is skipped and logged.
//! 5. Read it back. If Square stored anything besides the label, stop all
//!    writes, record the full before and after, and tell the owner.
//!
//! Writes go in batches, one batch at a time; the HTTP client backs off on
//! rate limits. A batch Square refuses is retried one item at a time so one
//! bad item can't block the rest.

pub mod diff;

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use dsf_compute::{LABEL_DESCRIPTION, LABEL_KEY, LABEL_NAME};
use dsf_square::api::Api;
use dsf_sync::cache::ts;
use dsf_sync::{Cache, CacheError, LabelPlan};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Catalog objects per write (an item counts once plus once per variation).
const BATCH_OBJECTS: usize = 400;
const ATTEMPTS: u32 = 3;

#[derive(Debug, thiserror::Error)]
pub enum LabelError {
    #[error(transparent)]
    Square(#[from] dsf_square::Error),
    #[error(transparent)]
    Cache(#[from] CacheError),
    #[error("Square hasn't given this app permission to write labels")]
    NoWriteScope,
    #[error("Square won't add another custom attribute to this account: {0}")]
    DefinitionLimit(String),
    #[error("label writes stopped: Square stored more than the label on {0}")]
    Halted(String),
}

impl From<rusqlite::Error> for LabelError {
    fn from(e: rusqlite::Error) -> Self {
        LabelError::Cache(CacheError::Sql(e))
    }
}

/// The evidence when a write changed more than the label.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Halt {
    pub object_id: String,
    pub name: String,
    pub at: DateTime<Utc>,
    pub paths: Vec<String>,
    pub fetched: Value,
    pub sent: Value,
    pub stored: Value,
}

#[derive(Debug, Default)]
pub struct LabelRun {
    pub written: u32,
    pub unchanged: u32,
    pub skipped: Vec<(String, String)>,
    pub halted: Option<Halt>,
    pub log: Vec<String>,
}

pub struct Writer<'a> {
    pub api: &'a Api<'a>,
    pub cache: &'a Cache,
    pub app_id: &'a str,
    pub now: DateTime<Utc>,
}

fn idempotency_key() -> String {
    let mut b = [0u8; 16];
    getrandom::fill(&mut b).expect("random source");
    format!("dsf-{}", b.iter().map(|x| format!("{x:02x}")).collect::<String>())
}

fn write_scope_missing(e: &dsf_square::Error) -> bool {
    e.status() == Some(403) || e.has_code("INSUFFICIENT_SCOPES")
}

/// Fatal for this check: Square can't be reached, is throttling past all
/// retries, or the token stopped working. Labels stay as they are.
fn fatal(e: &dsf_square::Error) -> bool {
    use dsf_square::Error as E;
    matches!(e, E::Network(_) | E::RateLimited | E::HostNotAllowed(_) | E::Decode(_))
        || e.is_unauthorized()
        || e.status().is_some_and(|s| s >= 500)
}

/// Sets or clears this app's label on one object (item or variation).
pub fn set_label(object: &mut Value, definition_id: &str, text: Option<&str>) {
    let Some(obj) = object.as_object_mut() else { return };
    match text {
        Some(text) => {
            let map =
                obj.entry("custom_attribute_values").or_insert_with(|| Value::Object(Default::default()));
            if !map.is_object() {
                *map = Value::Object(Default::default());
            }
            let map = map.as_object_mut().unwrap();
            match map.get_mut(LABEL_KEY).and_then(Value::as_object_mut) {
                Some(existing) => {
                    existing.insert("string_value".into(), json!(text));
                }
                None => {
                    map.insert(
                        LABEL_KEY.into(),
                        json!({
                            "name": LABEL_NAME,
                            "string_value": text,
                            "custom_attribute_definition_id": definition_id,
                            "key": LABEL_KEY,
                            "type": "STRING",
                        }),
                    );
                }
            }
        }
        None => {
            let empty = match obj.get_mut("custom_attribute_values").and_then(Value::as_object_mut) {
                Some(map) => {
                    map.shift_remove(LABEL_KEY);
                    map.is_empty()
                }
                None => false,
            };
            if empty {
                obj.shift_remove("custom_attribute_values");
            }
        }
    }
}

/// The outgoing object: the fetched item with only this app's labels set
/// as planned. Variations Square has that the plan doesn't know stay as
/// they are.
pub fn apply(fetched: &Value, plan: &LabelPlan, definition_id: &str) -> Value {
    let mut out = fetched.clone();
    set_label(&mut out, definition_id, plan.desired.as_deref());
    if let Some(vars) = out.pointer_mut("/item_data/variations").and_then(Value::as_array_mut) {
        for v in vars.iter_mut() {
            let id = v["id"].as_str().unwrap_or_default().to_string();
            if let Some(vp) = plan.variations.iter().find(|p| p.id == id) {
                set_label(v, definition_id, vp.desired.as_deref());
            }
        }
    }
    out
}

fn label_of(object: &Value) -> Option<String> {
    object.get("custom_attribute_values")?.get(LABEL_KEY)?.get("string_value")?.as_str().map(str::to_string)
}

impl Writer<'_> {
    /// Finds this app's definition, or creates it on first use.
    pub fn ensure_definition(&self) -> Result<String, LabelError> {
        if let Some(id) = self.cache.meta("definition_id")? {
            return Ok(id);
        }
        // The catalog sync records ours when it sees it; look once more
        // before creating, so a second definition is never made.
        let query = dsf_square::api::CatalogQuery {
            object_types: vec!["CUSTOM_ATTRIBUTE_DEFINITION"],
            begin_time: None,
            include_deleted: false,
        };
        let mut found = None;
        self.api.search_catalog(&query, |objects| -> Result<(), dsf_square::Error> {
            for o in objects {
                let d = &o["custom_attribute_definition_data"];
                if d["key"] == LABEL_KEY
                    && d.pointer("/source_application/application_id").and_then(Value::as_str)
                        == Some(self.app_id)
                {
                    found = o["id"].as_str().map(str::to_string);
                }
            }
            Ok(())
        })?;
        if let Some(id) = found {
            self.cache.set_meta("definition_id", &id)?;
            return Ok(id);
        }
        let definition = json!({
            "type": "CUSTOM_ATTRIBUTE_DEFINITION",
            "id": "#dead_stock_last_sold",
            "custom_attribute_definition_data": {
                "type": "STRING",
                "name": LABEL_NAME,
                "description": LABEL_DESCRIPTION,
                "allowed_object_types": ["ITEM", "ITEM_VARIATION"],
                "seller_visibility": "SELLER_VISIBILITY_READ_WRITE_VALUES",
                "app_visibility": "APP_VISIBILITY_HIDDEN",
                "key": LABEL_KEY,
                "string_config": { "enforce_uniqueness": false },
            },
        });
        match self.api.upsert(&idempotency_key(), definition) {
            Ok(created) => {
                let id = created["id"].as_str().unwrap_or_default().to_string();
                if id.is_empty() {
                    return Err(LabelError::Square(dsf_square::Error::Decode(
                        "definition created without an id".into(),
                    )));
                }
                self.cache.set_meta("definition_id", &id)?;
                Ok(id)
            }
            Err(e) if write_scope_missing(&e) => Err(LabelError::NoWriteScope),
            Err(e) if e.status() == Some(400) && e.to_string().to_lowercase().contains("maximum") => {
                Err(LabelError::DefinitionLimit(e.to_string()))
            }
            Err(e) => Err(LabelError::Square(e)),
        }
    }

    /// Writes every planned label that differs from Square's.
    pub fn write(
        &self,
        plans: &[LabelPlan],
        definition_id: &str,
        run: &mut LabelRun,
    ) -> Result<(), LabelError> {
        let pending: Vec<&LabelPlan> = plans.iter().filter(|p| p.needs_write()).collect();
        let mut batch: Vec<&LabelPlan> = Vec::new();
        let mut size = 0;
        for plan in pending {
            let objects = 1 + plan.variations.len();
            if !batch.is_empty() && size + objects > BATCH_OBJECTS {
                self.write_batch(&batch, definition_id, run)?;
                batch.clear();
                size = 0;
            }
            batch.push(plan);
            size += objects;
        }
        if !batch.is_empty() {
            self.write_batch(&batch, definition_id, run)?;
        }
        Ok(())
    }

    /// Clears every label this app wrote, by the same procedure, then
    /// deletes the definition if nothing was left behind.
    pub fn remove_all(&self, run: &mut LabelRun) -> Result<bool, LabelError> {
        let Some(definition_id) = self.cache.meta("definition_id")? else {
            return Ok(true);
        };
        let plans = dsf_sync::removal_plan(self.cache)?;
        self.write(&plans, &definition_id, run)?;
        if !run.skipped.is_empty() || run.halted.is_some() {
            return Ok(false);
        }
        match self.api.delete_object(&definition_id) {
            Ok(()) => {}
            Err(e) if e.status() == Some(404) => {}
            Err(e) => return Err(e.into()),
        }
        self.cache.clear_meta("definition_id")?;
        self.cache.conn().execute_batch(
            "UPDATE items SET label_value = NULL; UPDATE variations SET label_value = NULL; DELETE FROM label_skips;",
        )?;
        run.log.push("removed the Last sold attribute from Square".into());
        Ok(true)
    }

    fn skip(&self, run: &mut LabelRun, plan: &LabelPlan, reason: String) -> Result<(), LabelError> {
        self.cache.conn().execute(
            "INSERT INTO label_skips (object_id, reason, at) VALUES (?1, ?2, ?3)
             ON CONFLICT(object_id) DO UPDATE SET reason = excluded.reason, at = excluded.at",
            params![plan.item_id, reason, ts(self.now)],
        )?;
        run.log.push(format!("skipped {} ({}): {reason}", plan.item_id, plan.item_name));
        run.skipped.push((plan.item_id.clone(), reason));
        Ok(())
    }

    /// Records in the cache what Square now holds for an item.
    fn record(&self, stored: &Value) -> Result<(), LabelError> {
        let conn = self.cache.conn();
        let id = stored["id"].as_str().unwrap_or_default();
        conn.execute(
            "UPDATE items SET label_value = ?1, version = ?2 WHERE id = ?3",
            params![label_of(stored), stored["version"].as_i64(), id],
        )?;
        for v in stored.pointer("/item_data/variations").and_then(Value::as_array).into_iter().flatten() {
            conn.execute(
                "UPDATE variations SET label_value = ?1, version = ?2 WHERE id = ?3",
                params![label_of(v), v["version"].as_i64(), v["id"].as_str().unwrap_or_default()],
            )?;
        }
        conn.execute("DELETE FROM label_skips WHERE object_id = ?1", [id])?;
        Ok(())
    }

    fn write_batch(
        &self,
        plans: &[&LabelPlan],
        definition_id: &str,
        run: &mut LabelRun,
    ) -> Result<(), LabelError> {
        let ids: Vec<String> = plans.iter().map(|p| p.item_id.clone()).collect();
        let mut last_error = None;
        for attempt in 1..=ATTEMPTS {
            // 1. Fetch fresh.
            let fetched: HashMap<String, Value> = self
                .api
                .retrieve(&ids)?
                .into_iter()
                .map(|o| (o["id"].as_str().unwrap_or_default().to_string(), o))
                .collect();
            let mut outgoing: Vec<(&LabelPlan, Value, Value)> = Vec::new();
            for plan in plans {
                let Some(current) = fetched.get(&plan.item_id) else {
                    // Deleted since the sync: nothing to label.
                    run.log.push(format!("{} is no longer in Square", plan.item_id));
                    continue;
                };
                if current["type"] != "ITEM"
                    || current.get("is_deleted").and_then(Value::as_bool) == Some(true)
                {
                    continue;
                }
                // 2. Change only this app's key.
                let out = apply(current, plan, definition_id);
                if &out == current {
                    self.record(current)?;
                    run.unchanged += 1;
                    continue;
                }
                // 3. Prove it.
                if let Err(paths) = diff::only_label_changed(current, &out) {
                    self.skip(run, plan, format!("the write would have changed {}", paths.join(", ")))?;
                    continue;
                }
                outgoing.push((plan, current.clone(), out));
            }
            if outgoing.is_empty() {
                return Ok(());
            }
            // 4. Send with versions.
            let objects: Vec<Value> = outgoing.iter().map(|(_, _, out)| out.clone()).collect();
            match self.api.batch_upsert(&idempotency_key(), objects) {
                Ok(result) => return self.verify(&outgoing, result.objects, run),
                Err(e) if write_scope_missing(&e) => return Err(LabelError::NoWriteScope),
                Err(e) if fatal(&e) => return Err(e.into()),
                Err(e) if e.has_code("VERSION_MISMATCH") && attempt < ATTEMPTS => {
                    run.log.push(format!("version conflict (attempt {attempt}); fetching again"));
                    last_error = Some(e);
                    continue;
                }
                Err(e) => {
                    last_error = Some(e);
                    break;
                }
            }
        }
        let e = last_error.expect("loop ends with an error");
        if plans.len() > 1 {
            // Find the item Square refuses; the rest still get written.
            run.log.push(format!("batch of {} refused ({e}); writing one at a time", plans.len()));
            for plan in plans {
                self.write_batch(&[*plan], definition_id, run)?;
            }
            return Ok(());
        }
        self.skip(run, plans[0], e.to_string())
    }

    /// 5. Read back and compare.
    fn verify(
        &self,
        outgoing: &[(&LabelPlan, Value, Value)],
        returned: Vec<Value>,
        run: &mut LabelRun,
    ) -> Result<(), LabelError> {
        let returned: HashMap<String, Value> =
            returned.into_iter().map(|o| (o["id"].as_str().unwrap_or_default().to_string(), o)).collect();
        let ids: Vec<String> = outgoing.iter().map(|(p, _, _)| p.item_id.clone()).collect();
        let readback: HashMap<String, Value> = self
            .api
            .retrieve(&ids)?
            .into_iter()
            .map(|o| (o["id"].as_str().unwrap_or_default().to_string(), o))
            .collect();
        for (plan, fetched, sent) in outgoing {
            let echoed = returned.get(&plan.item_id);
            if let Some(echo) = echoed
                && let Err(paths) = diff::stored_as_sent(sent, echo)
            {
                return self.halt(run, plan, paths, fetched, sent, echo);
            }
            let Some(stored) = readback.get(&plan.item_id) else {
                run.log.push(format!("{} was deleted right after its label was written", plan.item_id));
                continue;
            };
            if let Err(paths) = diff::stored_as_sent(sent, stored) {
                if echoed.is_some() {
                    // Square stored exactly what was sent; the difference
                    // came from a later edit by someone else.
                    run.log.push(format!(
                        "{} changed again right after its label was written ({})",
                        plan.item_id,
                        paths.join(", ")
                    ));
                } else {
                    return self.halt(run, plan, paths, fetched, sent, stored);
                }
            }
            self.record(stored)?;
            run.written += 1;
        }
        Ok(())
    }

    fn halt(
        &self,
        run: &mut LabelRun,
        plan: &LabelPlan,
        paths: Vec<String>,
        fetched: &Value,
        sent: &Value,
        stored: &Value,
    ) -> Result<(), LabelError> {
        let halt = Halt {
            object_id: plan.item_id.clone(),
            name: plan.item_name.clone(),
            at: self.now,
            paths: paths.clone(),
            fetched: fetched.clone(),
            sent: sent.clone(),
            stored: stored.clone(),
        };
        self.cache.set_meta("writes_halted", &serde_json::to_string(&halt).unwrap_or_default())?;
        run.log.push(format!(
            "STOPPED all label writes: {} came back changed at {}",
            plan.item_id,
            paths.join(", ")
        ));
        run.halted = Some(halt);
        Err(LabelError::Halted(plan.item_name.clone()))
    }
}

/// The evidence of a stop, if label writes are stopped.
pub fn halted(cache: &Cache) -> Option<Halt> {
    cache.meta("writes_halted").ok().flatten().and_then(|h| serde_json::from_str(&h).ok())
}

/// The owner has seen the stop and wants labels to resume.
pub fn resume(cache: &Cache) -> Result<(), CacheError> {
    cache.clear_meta("writes_halted")
}
