//! 验证宿主运行设置的新增字段兼容与显式写入合同

use gateway_plugin_sdk::call::services::settings::{ReplaceRuntimeSettings, RuntimeSettings};
use serde_json::{Value, json};

fn legacy_settings() -> Value {
    json!({
        "request_profiles": {},
        "config_revision": 1,
        "request_location_enabled": false,
        "request_location": {
            "country": "US", "region": "", "city": "", "timezone": "UTC"
        },
        "model_mappings": {},
        "refresh_margin_seconds": 300,
        "refresh_concurrency": 2,
        "max_concurrent_per_account": 3,
        "request_interval_ms": 0,
        "max_waiting_per_key": 0,
        "max_waiting_per_account": 0,
        "concurrency_wait_timeout_seconds": 30,
        "openai_guardian_reserved_concurrency": 0,
        "responses_max_decompressed_body_bytes": 67_108_864,
        "smart_scheduling": {
            "loadWeight": 1.0, "quotaWeight": 1.0, "healthWeight": 1.0,
            "latencyWeight": 1.0, "resetWeight": 1.0, "queueWeight": 1.0,
            "preferHigherWeight": false
        },
        "rotation_strategy": "smart",
        "min_codex_desktop_version": null,
        "min_codex_cli_version": null,
        "usage_retention_days": 30,
        "ops_event_retention_days": 30,
        "audit_retention_days": 30,
        "account_auto_freeze_enabled": false,
        "account_auto_freeze_threshold": 3,
        "account_auto_freeze_window_seconds": 300,
        "account_auto_freeze_duration_seconds": 60,
        "account_auto_freeze_probe_enabled": false,
        "account_auto_freeze_probe_model": null,
        "account_auto_freeze_adaptive_concurrency": false,
        "account_warmup_enabled": false,
        "account_warmup_schedule_time": "00:00",
        "account_warmup_model": null,
        "updated_at": "2026-01-01T00:00:00Z"
    })
}

#[test]
fn loading_legacy_settings_uses_the_default_session_binding_ttl() {
    let settings: RuntimeSettings = serde_json::from_value(legacy_settings()).unwrap();
    assert_eq!(settings.openai_session_binding_ttl_hours, 24);
}

#[test]
fn replacing_loaded_settings_keeps_the_explicit_session_binding_ttl() {
    let mut value = legacy_settings();
    value["openai_session_binding_ttl_hours"] = json!(168);
    let settings: RuntimeSettings = serde_json::from_value(value).unwrap();
    let replacement = ReplaceRuntimeSettings::from(settings);
    assert_eq!(replacement.openai_session_binding_ttl_hours, Some(168));
    assert_eq!(
        serde_json::to_value(replacement).unwrap()["openai_session_binding_ttl_hours"],
        168,
    );
}

#[test]
fn replacing_legacy_settings_leaves_the_session_binding_ttl_unspecified() {
    let settings: RuntimeSettings = serde_json::from_value(legacy_settings()).unwrap();
    let mut value = serde_json::to_value(ReplaceRuntimeSettings::from(settings)).unwrap();
    value
        .as_object_mut()
        .unwrap()
        .remove("openai_session_binding_ttl_hours");
    let replacement: ReplaceRuntimeSettings = serde_json::from_value(value).unwrap();
    assert_eq!(replacement.openai_session_binding_ttl_hours, None);
    assert!(
        !serde_json::to_value(replacement)
            .unwrap()
            .as_object()
            .unwrap()
            .contains_key("openai_session_binding_ttl_hours"),
    );
}
