#![cfg(test)]

use super::{Database, Issue};

/// Create a temporary database for testing. Returns (Database, TempDir).
/// TempDir must be kept alive for the duration of the test.
pub fn test_db() -> (Database, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.db");
    let db = Database::open(&path).unwrap();
    (db, dir)
}

/// Create a minimal test issue with the given identifier and team.
pub fn make_issue(identifier: &str, team_key: &str) -> Issue {
    let id = uuid::Uuid::new_v4().to_string();
    Issue {
        id,
        identifier: identifier.to_string(),
        team_key: team_key.to_string(),
        title: format!("Test issue {identifier}"),
        description: Some(format!("Description for {identifier}")),
        state_name: "Todo".to_string(),
        state_type: "unstarted".to_string(),
        priority: 2,
        assignee_name: None,
        project_name: Some("TestProject".to_string()),
        labels_json: r#"["bug","ui"]"#.to_string(),
        created_at: "2026-01-01T00:00:00Z".to_string(),
        updated_at: "2026-01-02T00:00:00Z".to_string(),
        content_hash: "abc123".to_string(),
        synced_at: None,
        url: format!("https://linear.app/test/issue/{identifier}"),
        branch_name: None,
        workspace_id: "default".to_string(),
    }
}

/// Create a fake embedding blob (just zeroed bytes of the right size).
pub fn fake_embedding(dimensions: usize) -> Vec<u8> {
    vec![0u8; dimensions * 4] // f32 = 4 bytes each
}

use super::Label;

/// Create a minimal test label.
pub fn make_label(id: &str, name: &str, workspace_id: &str) -> Label {
    Label {
        id: id.to_string(),
        workspace_id: workspace_id.to_string(),
        name: name.to_string(),
        color: Some("#abcdef".to_string()),
        parent_id: None,
    }
}
