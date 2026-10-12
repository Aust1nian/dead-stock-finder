//! The Square calls the app makes, with paging. Each paged call hands one
//! page at a time to a callback, so a 20,000-item catalog or 100,000 orders
//! never sit in memory at once.
//!
//! Facts these calls rely on are in SQUARE_FACTS.md (Phase 0).

use serde::Deserialize;
use serde_json::{Value, json};

use crate::http::{Error, HttpClient, Method};

/// SearchOrders takes at most 10 locations per call.
pub const ORDER_LOCATIONS_PER_CALL: usize = 10;
pub const ORDERS_PAGE: u32 = 1000;
pub const CATALOG_PAGE: u32 = 1000;
pub const INVENTORY_PAGE: u32 = 1000;
/// BatchRetrieveCatalogObjects ids per call (items come with their variations).
pub const RETRIEVE_PER_CALL: usize = 100;
/// Retrieve calls in flight at once.
pub const RETRIEVE_PARALLEL: usize = 6;

/// One signed-in seller: the client and the current access token.
pub struct Api<'a> {
    client: &'a HttpClient,
    token: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Merchant {
    pub id: String,
    #[serde(default)]
    pub business_name: Option<String>,
    #[serde(default)]
    pub main_location_id: Option<String>,
    #[serde(default)]
    pub currency: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Location {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub timezone: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub currency: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Order {
    pub id: String,
    /// Orders not created through the API carry no version.
    #[serde(default)]
    pub version: Option<i64>,
    pub location_id: String,
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub closed_at: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
    #[serde(default)]
    pub line_items: Vec<LineItem>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LineItem {
    #[serde(default)]
    pub uid: Option<String>,
    #[serde(default)]
    pub catalog_object_id: Option<String>,
    #[serde(default)]
    pub quantity: String,
    #[serde(default)]
    pub item_type: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct InventoryCount {
    pub catalog_object_id: String,
    #[serde(default)]
    pub location_id: String,
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub quantity: String,
    #[serde(default)]
    pub calculated_at: Option<String>,
}

/// Which orders to read.
#[derive(Debug, Clone)]
pub enum OrderWindow {
    /// Completed orders closed in [start, end], oldest first (first sync).
    ClosedBetween { start: String, end: Option<String> },
    /// Completed orders updated at or after `start`, oldest first.
    UpdatedSince { start: String },
}

#[derive(Debug, Clone, Default)]
pub struct CatalogQuery {
    pub object_types: Vec<&'static str>,
    pub begin_time: Option<String>,
    pub include_deleted: bool,
}

pub struct UpsertResult {
    pub objects: Vec<Value>,
}

fn decode<T: for<'de> Deserialize<'de>>(v: Value, what: &str) -> Result<T, Error> {
    serde_json::from_value(v).map_err(|e| Error::Decode(format!("{what}: {e}")))
}

fn cursor_of(body: &Value) -> Option<String> {
    body.get("cursor").and_then(Value::as_str).filter(|c| !c.is_empty()).map(str::to_string)
}

impl<'a> Api<'a> {
    pub fn new(client: &'a HttpClient, access_token: &str) -> Self {
        Api { client, token: access_token.to_string() }
    }

    fn get(&self, path: &str) -> Result<Value, Error> {
        Ok(self.client.send(Method::Get, path, Some(&self.token), None)?.body)
    }

    fn post(&self, path: &str, body: &Value) -> Result<Value, Error> {
        Ok(self.client.send(Method::Post, path, Some(&self.token), Some(body))?.body)
    }

    pub fn merchant(&self) -> Result<Merchant, Error> {
        let body = self.get("/v2/merchants/me")?;
        decode(body.get("merchant").cloned().unwrap_or(Value::Null), "merchant")
    }

    pub fn locations(&self) -> Result<Vec<Location>, Error> {
        let body = self.get("/v2/locations")?;
        decode(body.get("locations").cloned().unwrap_or_else(|| json!([])), "locations")
    }

    /// SearchCatalogObjects, page by page. Returns `latest_time` if Square
    /// sent one.
    pub fn search_catalog<E: From<Error>>(
        &self,
        query: &CatalogQuery,
        mut on_page: impl FnMut(Vec<Value>) -> Result<(), E>,
    ) -> Result<Option<String>, E> {
        let mut cursor: Option<String> = None;
        let mut latest_time = None;
        loop {
            let mut body = json!({
                "object_types": query.object_types,
                "include_deleted_objects": query.include_deleted,
                "include_related_objects": false,
                "limit": CATALOG_PAGE,
            });
            if let Some(begin) = &query.begin_time {
                body["begin_time"] = json!(begin);
            }
            if let Some(c) = &cursor {
                body["cursor"] = json!(c);
            }
            let mut page = self.post("/v2/catalog/search", &body)?;
            if let Some(t) = page.get("latest_time").and_then(Value::as_str) {
                latest_time = Some(t.to_string());
            }
            cursor = cursor_of(&page);
            let objects = match page.get_mut("objects").map(Value::take) {
                Some(Value::Array(list)) => list,
                _ => Vec::new(),
            };
            on_page(objects)?;
            if cursor.is_none() {
                return Ok(latest_time);
            }
        }
    }

    /// BatchRetrieveCatalogObjects: the objects exactly as Square has them
    /// now, every field kept. [`RETRIEVE_PER_CALL`] ids per call, up to
    /// [`RETRIEVE_PARALLEL`] calls at once (reads; Square serializes only
    /// writes).
    pub fn retrieve(&self, ids: &[String]) -> Result<Vec<Value>, Error> {
        let chunks: Vec<&[String]> = ids.chunks(RETRIEVE_PER_CALL).collect();
        let mut out = Vec::new();
        for group in chunks.chunks(RETRIEVE_PARALLEL) {
            let results: Vec<Result<Vec<Value>, Error>> = std::thread::scope(|scope| {
                let handles: Vec<_> = group
                    .iter()
                    .map(|chunk| {
                        scope.spawn(move || -> Result<Vec<Value>, Error> {
                            let body = json!({ "object_ids": chunk, "include_related_objects": false });
                            let mut page = self.post("/v2/catalog/batch-retrieve", &body)?;
                            Ok(match page.get_mut("objects").map(Value::take) {
                                Some(Value::Array(list)) => list,
                                _ => Vec::new(),
                            })
                        })
                    })
                    .collect();
                handles.into_iter().map(|h| h.join().expect("retrieve thread")).collect()
            });
            for r in results {
                out.extend(r?);
            }
        }
        Ok(out)
    }

    pub fn batch_upsert(&self, idempotency_key: &str, objects: Vec<Value>) -> Result<UpsertResult, Error> {
        let body = json!({ "idempotency_key": idempotency_key, "batches": [{ "objects": objects }] });
        let mut response = self.post("/v2/catalog/batch-upsert", &body)?;
        let objects = match response.get_mut("objects").map(Value::take) {
            Some(Value::Array(list)) => list,
            _ => Vec::new(),
        };
        Ok(UpsertResult { objects })
    }

    pub fn upsert(&self, idempotency_key: &str, object: Value) -> Result<Value, Error> {
        let body = json!({ "idempotency_key": idempotency_key, "object": object });
        let mut response = self.post("/v2/catalog/object", &body)?;
        Ok(response.get_mut("catalog_object").map(Value::take).unwrap_or(Value::Null))
    }

    pub fn delete_object(&self, id: &str) -> Result<(), Error> {
        let path = format!("/v2/catalog/object/{}", url_segment(id));
        self.client.send(Method::Delete, &path, Some(&self.token), None)?;
        Ok(())
    }

    /// SearchOrders over any number of locations (10 per call), completed
    /// orders only, oldest first, page by page.
    pub fn search_orders<E: From<Error>>(
        &self,
        location_ids: &[String],
        window: &OrderWindow,
        mut on_page: impl FnMut(Vec<Order>) -> Result<(), E>,
    ) -> Result<(), E> {
        let (field, sort_field, start, end) = match window {
            OrderWindow::ClosedBetween { start, end } => ("closed_at", "CLOSED_AT", start, end.clone()),
            OrderWindow::UpdatedSince { start } => ("updated_at", "UPDATED_AT", start, None),
        };
        for chunk in location_ids.chunks(ORDER_LOCATIONS_PER_CALL) {
            let mut cursor: Option<String> = None;
            loop {
                let mut range = json!({ "start_at": start });
                if let Some(end) = &end {
                    range["end_at"] = json!(end);
                }
                let mut body = json!({
                    "location_ids": chunk,
                    "limit": ORDERS_PAGE,
                    "return_entries": false,
                    "query": {
                        "filter": {
                            "state_filter": { "states": ["COMPLETED"] },
                            "date_time_filter": { field: range },
                        },
                        "sort": { "sort_field": sort_field, "sort_order": "ASC" },
                    },
                });
                if let Some(c) = &cursor {
                    body["cursor"] = json!(c);
                }
                let mut page = self.post("/v2/orders/search", &body)?;
                cursor = cursor_of(&page);
                let orders = page.get_mut("orders").map(Value::take).unwrap_or_else(|| json!([]));
                on_page(decode::<Vec<Order>>(orders, "orders")?)?;
                if cursor.is_none() {
                    break;
                }
            }
        }
        Ok(())
    }

    /// In-stock counts at these locations, page by page.
    pub fn inventory_counts<E: From<Error>>(
        &self,
        location_ids: &[String],
        updated_after: Option<&str>,
        mut on_page: impl FnMut(Vec<InventoryCount>) -> Result<(), E>,
    ) -> Result<(), E> {
        let mut cursor: Option<String> = None;
        loop {
            let mut body = json!({
                "location_ids": location_ids,
                "states": ["IN_STOCK"],
                "limit": INVENTORY_PAGE,
            });
            if let Some(after) = updated_after {
                body["updated_after"] = json!(after);
            }
            if let Some(c) = &cursor {
                body["cursor"] = json!(c);
            }
            let mut page = self.post("/v2/inventory/counts/batch-retrieve", &body)?;
            cursor = cursor_of(&page);
            let counts = page.get_mut("counts").map(Value::take).unwrap_or_else(|| json!([]));
            on_page(decode::<Vec<InventoryCount>>(counts, "inventory counts")?)?;
            if cursor.is_none() {
                return Ok(());
            }
        }
    }
}

/// Catalog ids are alphanumeric, but encode anything else rather than let it
/// change the path.
fn url_segment(id: &str) -> String {
    id.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            other => format!("%{other:02X}"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_cannot_escape_the_path() {
        assert_eq!(url_segment("ABC123"), "ABC123");
        assert_eq!(url_segment("../x?y#z"), "..%2Fx%3Fy%23z");
    }
}
