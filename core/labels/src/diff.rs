//! The proof behind every write: two versions of a catalog object differ
//! only in this app's label key, at the item level and in its nested
//! variations, and nowhere else.
//!
//! Square's catalog writes replace the whole object, so the app always sends
//! back everything it fetched. These functions are the check that what it
//! sends is what it fetched plus its own label, and that what Square stored
//! is what it sent.

use dsf_compute::LABEL_KEY;
use serde_json::Value;

/// Removes this app's label (and an empty custom attribute map left behind)
/// at the item level and in every nested variation.
pub fn without_label(object: &Value) -> Value {
    let mut o = object.clone();
    strip(&mut o);
    if let Some(vars) = o.pointer_mut("/item_data/variations").and_then(Value::as_array_mut) {
        for v in vars.iter_mut() {
            strip(v);
        }
    }
    o
}

fn strip(o: &mut Value) {
    let Some(obj) = o.as_object_mut() else { return };
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

/// Removes the fields every write changes: version, updated_at.
pub fn without_write_stamps(object: &Value) -> Value {
    let mut o = object.clone();
    unstamp(&mut o);
    if let Some(vars) = o.pointer_mut("/item_data/variations").and_then(Value::as_array_mut) {
        for v in vars.iter_mut() {
            unstamp(v);
        }
    }
    o
}

fn unstamp(o: &mut Value) {
    if let Some(obj) = o.as_object_mut() {
        obj.shift_remove("version");
        obj.shift_remove("updated_at");
    }
}

/// JSON-pointer paths where `a` and `b` differ (for the log).
pub fn differences(a: &Value, b: &Value) -> Vec<String> {
    let mut out = Vec::new();
    walk(a, b, String::new(), &mut out);
    out
}

fn walk(a: &Value, b: &Value, path: String, out: &mut Vec<String>) {
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            for (k, va) in x {
                let p = format!("{path}/{}", escape(k));
                match y.get(k) {
                    Some(vb) => walk(va, vb, p, out),
                    None => out.push(p),
                }
            }
            for k in y.keys().filter(|k| !x.contains_key(*k)) {
                out.push(format!("{path}/{}", escape(k)));
            }
        }
        (Value::Array(x), Value::Array(y)) => {
            if x.len() != y.len() {
                out.push(format!("{path} (length {} vs {})", x.len(), y.len()));
                return;
            }
            for (i, (va, vb)) in x.iter().zip(y).enumerate() {
                walk(va, vb, format!("{path}/{i}"), out);
            }
        }
        _ if a != b => out.push(if path.is_empty() { "/".into() } else { path }),
        _ => {}
    }
}

fn escape(k: &str) -> String {
    k.replace('~', "~0").replace('/', "~1")
}

/// Step 3 of the safe write: the outgoing object is the fetched one with
/// only this app's label changed. Returns the offending paths otherwise.
pub fn only_label_changed(fetched: &Value, outgoing: &Value) -> Result<(), Vec<String>> {
    let a = without_label(fetched);
    let b = without_label(outgoing);
    if a == b { Ok(()) } else { Err(differences(&a, &b)) }
}

/// Step 5: what Square stored is what was sent, apart from the version and
/// timestamp every write changes.
pub fn stored_as_sent(sent: &Value, stored: &Value) -> Result<(), Vec<String>> {
    let a = without_write_stamps(sent);
    let b = without_write_stamps(stored);
    if a == b { Ok(()) } else { Err(differences(&a, &b)) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn item() -> Value {
        json!({
            "type": "ITEM", "id": "I1", "version": 5, "updated_at": "2026-01-01T00:00:00Z",
            "custom_attribute_values": { "Square:abc": { "string_value": "seller's own" } },
            "item_data": { "name": "Mug", "variations": [
                { "type": "ITEM_VARIATION", "id": "V1", "version": 5, "item_variation_data": { "name": "Blue", "price_money": { "amount": 1200, "currency": "USD" } } },
                { "type": "ITEM_VARIATION", "id": "V2", "version": 5, "item_variation_data": { "name": "Red" } }
            ] }
        })
    }

    #[test]
    fn label_changes_are_allowed() {
        let fetched = item();
        let mut out = fetched.clone();
        out["custom_attribute_values"][LABEL_KEY] = json!({ "string_value": "Last sold Jul 14, 2026" });
        out["item_data"]["variations"][1]["custom_attribute_values"] =
            json!({ LABEL_KEY: { "string_value": "x" } });
        assert!(only_label_changed(&fetched, &out).is_ok());
        // And removing them again.
        assert!(only_label_changed(&out, &fetched).is_ok());
    }

    #[test]
    fn anything_else_is_refused() {
        let fetched = item();
        type Change = Box<dyn Fn(&mut Value)>;
        let cases: Vec<Change> = vec![
            Box::new(|o| o["item_data"]["name"] = json!("Mug!")),
            Box::new(|o| o["version"] = json!(6)),
            Box::new(|o| {
                o["item_data"]["variations"].as_array_mut().unwrap().pop();
            }),
            Box::new(|o| o["custom_attribute_values"]["Square:abc"]["string_value"] = json!("changed")),
            Box::new(|o| {
                o["custom_attribute_values"].as_object_mut().unwrap().shift_remove("Square:abc");
            }),
            Box::new(|o| {
                o["item_data"]["variations"][0]["item_variation_data"]["price_money"]["amount"] = json!(1300)
            }),
            Box::new(|o| o["item_data"]["description"] = json!("")),
        ];
        for (i, change) in cases.iter().enumerate() {
            let mut out = fetched.clone();
            out["custom_attribute_values"][LABEL_KEY] = json!({ "string_value": "x" });
            change(&mut out);
            assert!(only_label_changed(&fetched, &out).is_err(), "case {i} should be refused");
        }
    }

    #[test]
    fn numbers_keep_their_exact_form() {
        // arbitrary_precision: 1.50 stays 1.50 through a round trip.
        let v: Value = serde_json::from_str(r#"{"q": 1.50, "big": 12345678901234567890}"#).unwrap();
        assert_eq!(serde_json::to_string(&v).unwrap(), r#"{"q":1.50,"big":12345678901234567890}"#);
    }

    #[test]
    fn stored_ignores_only_stamps() {
        let sent = item();
        let mut stored = sent.clone();
        stored["version"] = json!(9);
        stored["updated_at"] = json!("2026-02-02T00:00:00Z");
        stored["item_data"]["variations"][0]["version"] = json!(9);
        assert!(stored_as_sent(&sent, &stored).is_ok());
        stored["item_data"]["name"] = json!("Changed");
        assert_eq!(stored_as_sent(&sent, &stored).unwrap_err(), vec!["/item_data/name".to_string()]);
    }
}
