//! Tests for `ProxyParams` extraction + emission (Q25 V2/V3). Included into
//! `params.rs` via `#[path]` to keep each file within the structural LOC limit.

use super::*;
use serde_json::json;

fn param_rich() -> Value {
    json!({
        "model": "claude-sonnet-4-6",
        "max_tokens": 32000,
        "temperature": 0.7,
        "top_p": 0.9,
        "top_k": 40,
        "stream": true,
        "thinking": {"type": "enabled", "budget_tokens": 10000},
        "system": "you are helpful",
        "messages": [{"role": "user"}, {"role": "assistant"}, {"role": "user"}],
        "tools": [{}, {}, {}, {}, {}],
        "stop_sequences": ["STOP", "END"],
        "tool_choice": {"type": "auto"}
    })
}

#[test]
fn param_rich_sets_every_field_native_typed() {
    let p = ProxyParams::extract(Some(&param_rich()));
    assert_eq!(p.model.as_deref(), Some("claude-sonnet-4-6"));
    assert_eq!(p.thinking_enabled, Some(true));
    assert_eq!(p.thinking_budget_tokens, Some(10000));
    assert_eq!(p.max_tokens, Some(32000));
    assert_eq!(p.temperature, Some(0.7));
    assert_eq!(p.top_p, Some(0.9));
    assert_eq!(p.top_k, Some(40));
    assert_eq!(p.stream, Some(true));
    assert_eq!(p.stop_sequences_count, Some(2));
    assert_eq!(p.system_present, Some(true));
    assert_eq!(p.tools_count, Some(5));
    assert_eq!(p.messages_count, Some(3));
    assert_eq!(p.tool_choice.as_deref(), Some("auto"));
    assert!(!p.body_parse_error);
}

#[test]
fn adaptive_thinking_enabled_with_null_budget() {
    // G1 real-wire shape: thinking is {type: adaptive} with no budget_tokens.
    let body = json!({"model": "claude-sonnet-4-6", "thinking": {"type": "adaptive"}});
    let p = ProxyParams::extract(Some(&body));
    assert_eq!(p.thinking_enabled, Some(true));
    assert_eq!(p.thinking_budget_tokens, None);
}

#[test]
fn disabled_thinking_reads_false() {
    let body = json!({"thinking": {"type": "disabled"}});
    let p = ProxyParams::extract(Some(&body));
    assert_eq!(p.thinking_enabled, Some(false));
}

#[test]
fn minimal_body_sets_three_fields_rest_null() {
    // RUNBOOK E2E shape: model + max_tokens + messages only.
    let body = json!({"model": "claude-haiku-4-5", "max_tokens": 1024, "messages": [{}]});
    let p = ProxyParams::extract(Some(&body));
    assert_eq!(p.model.as_deref(), Some("claude-haiku-4-5"));
    assert_eq!(p.max_tokens, Some(1024));
    assert_eq!(p.messages_count, Some(1));
    assert_eq!(p.thinking_enabled, None);
    assert_eq!(p.temperature, None);
    assert_eq!(p.tools_count, None);
    assert_eq!(p.system_present, Some(false));
    assert!(!p.body_parse_error);
}

#[test]
fn none_body_all_null() {
    let p = ProxyParams::extract(None);
    assert_eq!(p, ProxyParams::default());
    assert!(!p.body_parse_error);
}

#[test]
fn shape_divergent_body_degrades_to_null_and_flags() {
    // max_tokens as string, thinking as string — present-but-wrong-type.
    let body = json!({"max_tokens": "32000", "thinking": "lots"});
    let p = ProxyParams::extract(Some(&body));
    assert_eq!(p.max_tokens, None);
    assert_eq!(p.thinking_enabled, None);
    assert!(
        p.body_parse_error,
        "shape divergence must set body_parse_error"
    );
}

#[test]
fn param_rich_and_minimal_field_sets_differ() {
    // AP-07: distinct inputs must produce distinct field sets (delta != 0).
    let rich = ProxyParams::extract(Some(&param_rich()));
    let minimal = ProxyParams::extract(Some(
        &json!({"model": "m", "max_tokens": 8, "messages": [{}]}),
    ));
    assert_ne!(rich, minimal);
    assert_ne!(rich.tools_count, minimal.tools_count);
    assert_ne!(rich.temperature, minimal.temperature);
}

#[test]
fn write_into_emits_native_typed_values() {
    // V3: numeric/bool/float render as native JSON, not stringified.
    let p = ProxyParams::extract(Some(&param_rich()));
    let mut map = Map::new();
    p.write_into(&mut map);
    assert!(
        map["proxy.max_tokens"].is_i64(),
        "max_tokens must be a JSON int"
    );
    assert!(
        map["proxy.stream"].is_boolean(),
        "stream must be a JSON bool"
    );
    assert!(
        map["proxy.temperature"].is_f64(),
        "temperature must be a JSON float"
    );
    assert_eq!(map["proxy.tools_count"], json!(5));
    assert_eq!(map["proxy.model"], json!("claude-sonnet-4-6"));
    assert_eq!(map["proxy.body_parse_error"], json!(false));
    // Absent optional renders as JSON null, not omitted.
    let mut nmap = Map::new();
    ProxyParams::default().write_into(&mut nmap);
    assert!(nmap["proxy.temperature"].is_null());
}
