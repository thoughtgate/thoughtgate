//! Dashboard JSON validation tests.
//!
//! Ensures the Grafana dashboard JSON files are valid and have required structure.
//!
//! # Traceability
//! - Implements: REQ-OBS-002 §10.1, §10.2 (Dashboard specifications)

#[test]
fn test_overview_dashboard_valid_json() {
    let overview = include_str!("../../deploy/grafana/thoughtgate-overview.json");

    let dashboard: serde_json::Value =
        serde_json::from_str(overview).expect("overview dashboard should be valid JSON");

    // Verify required fields
    assert!(
        dashboard["title"].is_string(),
        "overview dashboard missing title"
    );
    assert_eq!(
        dashboard["title"].as_str().unwrap(),
        "ThoughtGate Overview",
        "overview dashboard wrong title"
    );
    assert_eq!(
        dashboard["uid"].as_str().unwrap(),
        "thoughtgate-overview",
        "overview dashboard wrong UID"
    );

    // Verify panel count (§10.1 specifies 8 panels)
    let panels = dashboard["panels"]
        .as_array()
        .expect("overview dashboard missing panels");
    assert!(
        panels.len() >= 8,
        "overview dashboard should have at least 8 panels, found {}",
        panels.len()
    );

    // Verify all non-row panels have targets (queries)
    for (i, panel) in panels.iter().enumerate() {
        let panel_type = panel["type"].as_str().unwrap_or("unknown");
        if panel_type != "row" {
            assert!(
                panel["targets"].is_array(),
                "overview panel {} ({}) missing targets",
                i,
                panel["title"].as_str().unwrap_or("untitled")
            );
            let targets = panel["targets"].as_array().unwrap();
            assert!(
                !targets.is_empty(),
                "overview panel {} ({}) has empty targets",
                i,
                panel["title"].as_str().unwrap_or("untitled")
            );
        }
    }

    // Verify datasource template variable usage
    let expected_ds_uid = "${DS_PROMETHEUS}";
    for panel in panels {
        if let Some(ds) = panel["datasource"].as_object() {
            assert_eq!(
                ds.get("uid").and_then(|u| u.as_str()),
                Some(expected_ds_uid),
                "panel should use ${{DS_PROMETHEUS}} datasource template"
            );
        }
    }
}

#[test]
fn test_policies_dashboard_valid_json() {
    let policies = include_str!("../../deploy/grafana/thoughtgate-policies.json");

    let dashboard: serde_json::Value =
        serde_json::from_str(policies).expect("policies dashboard should be valid JSON");

    // Verify required fields
    assert!(
        dashboard["title"].is_string(),
        "policies dashboard missing title"
    );
    assert_eq!(
        dashboard["title"].as_str().unwrap(),
        "ThoughtGate Policies",
        "policies dashboard wrong title"
    );
    assert_eq!(
        dashboard["uid"].as_str().unwrap(),
        "thoughtgate-policies",
        "policies dashboard wrong UID"
    );

    // Verify panel count (§10.2 specifies 6 panels)
    let panels = dashboard["panels"]
        .as_array()
        .expect("policies dashboard missing panels");
    assert!(
        panels.len() >= 6,
        "policies dashboard should have at least 6 panels, found {}",
        panels.len()
    );

    // Verify all non-row panels have targets (queries)
    for (i, panel) in panels.iter().enumerate() {
        let panel_type = panel["type"].as_str().unwrap_or("unknown");
        if panel_type != "row" {
            assert!(
                panel["targets"].is_array(),
                "policies panel {} ({}) missing targets",
                i,
                panel["title"].as_str().unwrap_or("untitled")
            );
            let targets = panel["targets"].as_array().unwrap();
            assert!(
                !targets.is_empty(),
                "policies panel {} ({}) has empty targets",
                i,
                panel["title"].as_str().unwrap_or("untitled")
            );
        }
    }
}

#[test]
fn test_dashboards_have_expected_panels() {
    // Overview dashboard panels
    let overview = include_str!("../../deploy/grafana/thoughtgate-overview.json");
    let overview_dash: serde_json::Value = serde_json::from_str(overview).unwrap();
    let overview_panels = overview_dash["panels"].as_array().unwrap();

    let overview_titles: Vec<&str> = overview_panels
        .iter()
        .filter_map(|p| p["title"].as_str())
        .collect();

    // §10.1 required panels
    let expected_overview = [
        "Request Rate",
        "Error Rate %",
        "Latency Percentiles",
        "Cedar Decisions",
        "Active Connections",
        "Pending Approvals",
        "Top Tools",
        "Upstream Latency p95",
    ];

    for expected in expected_overview {
        assert!(
            overview_titles.contains(&expected),
            "Overview dashboard missing required panel: {}",
            expected
        );
    }

    // Policies dashboard panels
    let policies = include_str!("../../deploy/grafana/thoughtgate-policies.json");
    let policies_dash: serde_json::Value = serde_json::from_str(policies).unwrap();
    let policies_panels = policies_dash["panels"].as_array().unwrap();

    let policies_titles: Vec<&str> = policies_panels
        .iter()
        .filter_map(|p| p["title"].as_str())
        .collect();

    // §10.2 required panels
    let expected_policies = [
        "Cedar Evaluation Latency",
        "Denials by Policy",
        "Denials by Tool",
        "Approval Wait Times",
        "Approval Outcomes",
        "Gate Flow Funnel",
    ];

    for expected in expected_policies {
        assert!(
            policies_titles.contains(&expected),
            "Policies dashboard missing required panel: {}",
            expected
        );
    }
}

#[test]
fn test_dashboards_refresh_and_time_settings() {
    let overview = include_str!("../../deploy/grafana/thoughtgate-overview.json");
    let policies = include_str!("../../deploy/grafana/thoughtgate-policies.json");

    for (name, json) in [("overview", overview), ("policies", policies)] {
        let dashboard: serde_json::Value = serde_json::from_str(json).unwrap();

        // Verify 30s refresh
        assert_eq!(
            dashboard["refresh"].as_str(),
            Some("30s"),
            "{} dashboard should have 30s refresh",
            name
        );

        // Verify 1h time range
        assert_eq!(
            dashboard["time"]["from"].as_str(),
            Some("now-1h"),
            "{} dashboard should have 1h default range",
            name
        );
    }
}
