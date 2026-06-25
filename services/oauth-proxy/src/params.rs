//! Content-free request-parameter capture for proxy spans (Q25).
//!
//! Reads native-typed, content-free model request parameters off the pristine
//! pre-mutation `parsed_body` (`serde_json::Value`) and exposes them for the
//! `metadata.proxy.*` span attributes. No request/response content is captured:
//! only scalars, enums, presence flags, and counts.
//!
//! `proxy.effort` is intentionally absent: G1's wire capture showed no top-level
//! effort/reasoning key in the Anthropic Messages API, and modern Claude Code
//! sends `thinking: {type: "adaptive"}` with no `budget_tokens`, so a derived
//! effort would be permanently null. See Q25-gate.md §Notes.

use serde_json::{Map, Value};

/// Captured request parameters. Every field is nullable — an absent or
/// shape-divergent body field stays `None` (no fabricated default).
#[derive(Debug, Default, Clone, PartialEq)]
pub struct ProxyParams {
    pub model: Option<String>,
    pub thinking_enabled: Option<bool>,
    pub thinking_budget_tokens: Option<i64>,
    pub max_tokens: Option<i64>,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub top_k: Option<i64>,
    pub stream: Option<bool>,
    pub stop_sequences_count: Option<i64>,
    pub system_present: Option<bool>,
    pub tools_count: Option<i64>,
    pub tool_choice: Option<String>,
    pub messages_count: Option<i64>,
    /// Set when a present body field has an unexpected shape (e.g. `max_tokens`
    /// as a string). Never converts a request into an error.
    pub body_parse_error: bool,
}

impl ProxyParams {
    /// Extract parameters from the pristine `parsed_body`. `None` (passthrough
    /// mode) or a non-object body yields all-null fields.
    pub fn extract(body: Option<&Value>) -> Self {
        let Some(Value::Object(obj)) = body else {
            return Self::default();
        };
        let mut err = false;
        let (thinking_enabled, thinking_budget_tokens) = thinking_fields(obj, &mut err);
        Self {
            model: str_field(obj, "model", &mut err),
            thinking_enabled,
            thinking_budget_tokens,
            max_tokens: int_field(obj, "max_tokens", &mut err),
            temperature: float_field(obj, "temperature", &mut err),
            top_p: float_field(obj, "top_p", &mut err),
            top_k: int_field(obj, "top_k", &mut err),
            stream: bool_field(obj, "stream", &mut err),
            stop_sequences_count: len_field(obj, "stop_sequences", &mut err),
            system_present: Some(obj.contains_key("system")),
            tools_count: len_field(obj, "tools", &mut err),
            tool_choice: tool_choice_field(obj, &mut err),
            messages_count: len_field(obj, "messages", &mut err),
            body_parse_error: err,
        }
    }

    /// Insert the `proxy.*` entries as native-typed JSON values into a metadata
    /// map. Absent optionals are written as JSON `null`.
    pub fn write_into(&self, map: &mut Map<String, Value>) {
        map.insert("proxy.model".into(), opt_str(&self.model));
        map.insert(
            "proxy.thinking_enabled".into(),
            opt_bool(self.thinking_enabled),
        );
        map.insert(
            "proxy.thinking_budget_tokens".into(),
            opt_int(self.thinking_budget_tokens),
        );
        map.insert("proxy.max_tokens".into(), opt_int(self.max_tokens));
        map.insert("proxy.temperature".into(), opt_float(self.temperature));
        map.insert("proxy.top_p".into(), opt_float(self.top_p));
        map.insert("proxy.top_k".into(), opt_int(self.top_k));
        map.insert("proxy.stream".into(), opt_bool(self.stream));
        map.insert(
            "proxy.stop_sequences_count".into(),
            opt_int(self.stop_sequences_count),
        );
        map.insert("proxy.system_present".into(), opt_bool(self.system_present));
        map.insert("proxy.tools_count".into(), opt_int(self.tools_count));
        map.insert("proxy.tool_choice".into(), opt_str(&self.tool_choice));
        map.insert("proxy.messages_count".into(), opt_int(self.messages_count));
        map.insert(
            "proxy.body_parse_error".into(),
            Value::Bool(self.body_parse_error),
        );
    }
}

/// thinking_enabled = block present AND type != "disabled"; budget from
/// `budget_tokens`. A non-object `thinking` is shape divergence.
fn thinking_fields(obj: &Map<String, Value>, err: &mut bool) -> (Option<bool>, Option<i64>) {
    match obj.get("thinking") {
        None => (None, None),
        Some(Value::Object(t)) => {
            let enabled = match t.get("type") {
                Some(Value::String(s)) => Some(s != "disabled"),
                Some(_) => {
                    *err = true;
                    None
                }
                None => None,
            };
            (enabled, int_field(t, "budget_tokens", err))
        }
        Some(_) => {
            *err = true;
            (None, None)
        }
    }
}

/// tool_choice captures only the `.type` enum string — never tool schemas.
fn tool_choice_field(obj: &Map<String, Value>, err: &mut bool) -> Option<String> {
    match obj.get("tool_choice") {
        None => None,
        Some(Value::Object(tc)) => match tc.get("type") {
            Some(Value::String(s)) => Some(s.clone()),
            Some(_) => {
                *err = true;
                None
            }
            None => None,
        },
        Some(_) => {
            *err = true;
            None
        }
    }
}

fn str_field(obj: &Map<String, Value>, key: &str, err: &mut bool) -> Option<String> {
    match obj.get(key) {
        None => None,
        Some(Value::String(s)) => Some(s.clone()),
        Some(_) => mark(err),
    }
}

fn int_field(obj: &Map<String, Value>, key: &str, err: &mut bool) -> Option<i64> {
    match obj.get(key) {
        None => None,
        Some(v) => v.as_i64().or_else(|| mark(err)),
    }
}

fn float_field(obj: &Map<String, Value>, key: &str, err: &mut bool) -> Option<f64> {
    match obj.get(key) {
        None => None,
        Some(v) => v.as_f64().or_else(|| mark(err)),
    }
}

fn bool_field(obj: &Map<String, Value>, key: &str, err: &mut bool) -> Option<bool> {
    match obj.get(key) {
        None => None,
        Some(Value::Bool(b)) => Some(*b),
        Some(_) => mark(err),
    }
}

/// Length of an array field (count only — never the elements' content).
fn len_field(obj: &Map<String, Value>, key: &str, err: &mut bool) -> Option<i64> {
    match obj.get(key) {
        None => None,
        Some(Value::Array(a)) => Some(a.len() as i64),
        Some(_) => mark(err),
    }
}

/// Record a shape divergence and yield `None` for the field.
fn mark<T>(err: &mut bool) -> Option<T> {
    *err = true;
    None
}

fn opt_str(v: &Option<String>) -> Value {
    v.clone().map(Value::String).unwrap_or(Value::Null)
}
fn opt_bool(v: Option<bool>) -> Value {
    v.map(Value::Bool).unwrap_or(Value::Null)
}
fn opt_int(v: Option<i64>) -> Value {
    v.map(Value::from).unwrap_or(Value::Null)
}
fn opt_float(v: Option<f64>) -> Value {
    v.map(Value::from).unwrap_or(Value::Null)
}

#[cfg(test)]
#[path = "params_tests.rs"]
mod tests;
