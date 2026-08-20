//! Query-less issue browsing: filter by team/state/labels/date windows,
//! ordered by recency. This is the read path for "what changed lately"
//! questions that have no text to search for.

use anyhow::Result;

use super::{Database, Issue};

/// Date-window predicates applied to `issues.updated_at` / `issues.created_at`.
/// All values must be normalized RFC 3339 UTC timestamps (see `crate::dates`);
/// comparisons are inclusive on `after` bounds and exclusive on `before` bounds.
#[derive(Debug, Default, Clone)]
pub struct DateFilters {
    pub updated_after: Option<String>,
    pub updated_before: Option<String>,
    pub created_after: Option<String>,
    pub created_before: Option<String>,
}

impl DateFilters {
    pub fn is_empty(&self) -> bool {
        self.updated_after.is_none()
            && self.updated_before.is_none()
            && self.created_after.is_none()
            && self.created_before.is_none()
    }
}

/// Sort order for `list_issues`.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum ListOrder {
    /// Most recently updated first (the default: recency browsing).
    #[default]
    UpdatedDesc,
    /// Most recently created first.
    CreatedDesc,
    /// Urgent first (priority 1), then high..low, no-priority (0) last;
    /// ties broken by most recently updated.
    Priority,
}

impl std::str::FromStr for ListOrder {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self> {
        match s.to_lowercase().as_str() {
            "updated" | "updated_desc" => Ok(Self::UpdatedDesc),
            "created" | "created_desc" => Ok(Self::CreatedDesc),
            "priority" => Ok(Self::Priority),
            _ => anyhow::bail!(
                "Invalid order '{s}'. Use 'updated' (default), 'created', or 'priority'"
            ),
        }
    }
}

/// Parameters for [`Database::list_issues`].
#[derive(Debug, Default)]
pub struct ListIssuesParams<'a> {
    pub workspace_id: &'a str,
    pub team_key: Option<&'a str>,
    /// Case-insensitive substring match on `state_name` (same semantics as search).
    pub state_filter: Option<&'a str>,
    /// Issues must carry ALL of these labels.
    pub label_ids: Option<&'a [String]>,
    pub dates: DateFilters,
    pub order: ListOrder,
    pub limit: usize,
    pub offset: usize,
    /// Archived issues are excluded unless set.
    pub include_archived: bool,
}

const ISSUE_COLUMNS: &str = "id, identifier, team_key, title, description, state_name, state_type, \
     priority, assignee_name, project_name, labels_json, created_at, updated_at, \
     content_hash, synced_at, url, branch_name, workspace_id, project_id, \
     project_milestone_id, project_milestone_name, cycle_id, cycle_name, archived_at, due_date";

impl Database {
    /// Browse issues from the local store without a search query.
    pub fn list_issues(&self, params: &ListIssuesParams<'_>) -> Result<Vec<Issue>> {
        self.with_conn(|conn| {
            let mut sql_params: Vec<Box<dyn rusqlite::types::ToSql>> =
                vec![Box::new(params.workspace_id.to_string())];
            let mut clauses = vec!["workspace_id = ?1".to_string()];

            let push = |value: String, clauses: &mut Vec<String>,
                            sql_params: &mut Vec<Box<dyn rusqlite::types::ToSql>>,
                            template: &str| {
                sql_params.push(Box::new(value));
                clauses.push(template.replace("{n}", &sql_params.len().to_string()));
            };

            if let Some(team) = params.team_key {
                push(team.to_string(), &mut clauses, &mut sql_params, "team_key = ?{n}");
            }
            if let Some(state) = params.state_filter {
                push(
                    format!("%{}%", state.to_lowercase()),
                    &mut clauses,
                    &mut sql_params,
                    "LOWER(state_name) LIKE ?{n}",
                );
            }
            let d = &params.dates;
            if let Some(v) = &d.updated_after {
                push(v.clone(), &mut clauses, &mut sql_params, "updated_at >= ?{n}");
            }
            if let Some(v) = &d.updated_before {
                push(v.clone(), &mut clauses, &mut sql_params, "updated_at < ?{n}");
            }
            if let Some(v) = &d.created_after {
                push(v.clone(), &mut clauses, &mut sql_params, "created_at >= ?{n}");
            }
            if let Some(v) = &d.created_before {
                push(v.clone(), &mut clauses, &mut sql_params, "created_at < ?{n}");
            }
            if !params.include_archived {
                clauses.push("archived_at IS NULL".to_string());
            }
            if let Some(ids) = params.label_ids.filter(|ids| !ids.is_empty()) {
                let (frag, mut lp) =
                    Self::label_filter_fragment(ids, sql_params.len() + 1, "issues");
                sql_params.append(&mut lp);
                clauses.push(frag);
            }

            // Trailing `id` makes ordering total: timestamps and priorities tie
            // constantly, and without a deterministic final key LIMIT/OFFSET
            // pages could reorder, duplicate, or skip tied records.
            let order = match params.order {
                ListOrder::UpdatedDesc => "updated_at DESC, id",
                ListOrder::CreatedDesc => "created_at DESC, id",
                // Priority 0 means "no priority" in Linear; sort it after 1..4.
                ListOrder::Priority => {
                    "CASE WHEN priority = 0 THEN 5 ELSE priority END ASC, updated_at DESC, id"
                }
            };

            sql_params.push(Box::new(params.limit as i64));
            let limit_idx = sql_params.len();
            sql_params.push(Box::new(params.offset as i64));
            let offset_idx = sql_params.len();

            let where_clause = clauses.join(" AND ");
            let sql = format!(
                "SELECT {ISSUE_COLUMNS} FROM issues WHERE {where_clause} \
                 ORDER BY {order} LIMIT ?{limit_idx} OFFSET ?{offset_idx}"
            );

            let mut stmt = conn.prepare(&sql)?;
            let param_refs: Vec<&dyn rusqlite::types::ToSql> =
                sql_params.iter().map(|p| p.as_ref()).collect();
            let rows = stmt.query_map(param_refs.as_slice(), Issue::from_row)?;
            Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_helpers::{make_issue, make_label, test_db};

    /// Insert an issue with controlled timestamps.
    fn seed(
        db: &Database,
        identifier: &str,
        team: &str,
        created_at: &str,
        updated_at: &str,
    ) -> Issue {
        let mut issue = make_issue(identifier, team);
        issue.created_at = created_at.to_string();
        issue.updated_at = updated_at.to_string();
        db.upsert_issue(&issue).unwrap();
        issue
    }

    fn ids(results: &[Issue]) -> Vec<&str> {
        results.iter().map(|i| i.identifier.as_str()).collect()
    }

    fn base_params(workspace_id: &str) -> ListIssuesParams<'_> {
        ListIssuesParams {
            workspace_id,
            limit: 50,
            ..Default::default()
        }
    }

    #[test]
    fn lists_all_issues_most_recently_updated_first() {
        let (db, _dir) = test_db();
        seed(&db, "HPN-1", "HPN", "2026-08-01T00:00:00Z", "2026-08-10T00:00:00Z");
        seed(&db, "HPN-2", "HPN", "2026-08-02T00:00:00Z", "2026-08-15T00:00:00Z");
        seed(&db, "HPN-3", "HPN", "2026-08-03T00:00:00Z", "2026-08-12T00:00:00Z");

        let results = db.list_issues(&base_params("default")).unwrap();
        assert_eq!(ids(&results), vec!["HPN-2", "HPN-3", "HPN-1"]);
    }

    #[test]
    fn updated_after_is_inclusive_and_updated_before_is_exclusive() {
        let (db, _dir) = test_db();
        seed(&db, "HPN-1", "HPN", "2026-08-01T00:00:00Z", "2026-08-10T00:00:00Z");
        seed(&db, "HPN-2", "HPN", "2026-08-01T00:00:00Z", "2026-08-15T00:00:00Z");
        seed(&db, "HPN-3", "HPN", "2026-08-01T00:00:00Z", "2026-08-20T00:00:00Z");

        let mut params = base_params("default");
        params.dates.updated_after = Some("2026-08-15T00:00:00Z".to_string());
        let results = db.list_issues(&params).unwrap();
        assert_eq!(ids(&results), vec!["HPN-3", "HPN-2"]);

        let mut params = base_params("default");
        params.dates.updated_before = Some("2026-08-15T00:00:00Z".to_string());
        let results = db.list_issues(&params).unwrap();
        assert_eq!(ids(&results), vec!["HPN-1"]);
    }

    #[test]
    fn created_window_filters_independently_of_updated() {
        let (db, _dir) = test_db();
        seed(&db, "HPN-1", "HPN", "2026-06-01T00:00:00Z", "2026-08-19T00:00:00Z");
        seed(&db, "HPN-2", "HPN", "2026-08-10T00:00:00Z", "2026-08-11T00:00:00Z");

        let mut params = base_params("default");
        params.dates.created_after = Some("2026-08-01T00:00:00Z".to_string());
        let results = db.list_issues(&params).unwrap();
        assert_eq!(ids(&results), vec!["HPN-2"]);
    }

    #[test]
    fn empty_window_returns_no_results() {
        let (db, _dir) = test_db();
        seed(&db, "HPN-1", "HPN", "2026-08-01T00:00:00Z", "2026-08-10T00:00:00Z");

        let mut params = base_params("default");
        params.dates.updated_after = Some("2026-09-01T00:00:00Z".to_string());
        assert!(db.list_issues(&params).unwrap().is_empty());
    }

    #[test]
    fn filters_by_team_and_workspace() {
        let (db, _dir) = test_db();
        seed(&db, "HPN-1", "HPN", "2026-08-01T00:00:00Z", "2026-08-10T00:00:00Z");
        seed(&db, "ENG-1", "ENG", "2026-08-01T00:00:00Z", "2026-08-11T00:00:00Z");
        let mut other = make_issue("OTH-1", "HPN");
        other.workspace_id = "other".to_string();
        db.upsert_issue(&other).unwrap();

        let mut params = base_params("default");
        params.team_key = Some("HPN");
        let results = db.list_issues(&params).unwrap();
        assert_eq!(ids(&results), vec!["HPN-1"]);
    }

    #[test]
    fn state_filter_is_case_insensitive_substring() {
        let (db, _dir) = test_db();
        let mut a = make_issue("HPN-1", "HPN");
        a.state_name = "In Progress".to_string();
        db.upsert_issue(&a).unwrap();
        let mut b = make_issue("HPN-2", "HPN");
        b.state_name = "Done".to_string();
        db.upsert_issue(&b).unwrap();

        let mut params = base_params("default");
        params.state_filter = Some("progress");
        let results = db.list_issues(&params).unwrap();
        assert_eq!(ids(&results), vec!["HPN-1"]);
    }

    #[test]
    fn label_filter_requires_all_labels() {
        let (db, _dir) = test_db();
        let a = seed(&db, "HPN-1", "HPN", "2026-08-01T00:00:00Z", "2026-08-10T00:00:00Z");
        let b = seed(&db, "HPN-2", "HPN", "2026-08-01T00:00:00Z", "2026-08-11T00:00:00Z");
        db.upsert_label(&make_label("l1", "bug", "default")).unwrap();
        db.upsert_label(&make_label("l2", "ui", "default")).unwrap();
        db.replace_issue_labels(&a.id, &["l1".to_string(), "l2".to_string()]).unwrap();
        db.replace_issue_labels(&b.id, &["l1".to_string()]).unwrap();

        let mut params = base_params("default");
        let wanted = vec!["l1".to_string(), "l2".to_string()];
        params.label_ids = Some(&wanted);
        let results = db.list_issues(&params).unwrap();
        assert_eq!(ids(&results), vec!["HPN-1"]);
    }

    #[test]
    fn combines_labels_with_date_window() {
        let (db, _dir) = test_db();
        let a = seed(&db, "HPN-1", "HPN", "2026-08-01T00:00:00Z", "2026-08-10T00:00:00Z");
        let b = seed(&db, "HPN-2", "HPN", "2026-08-01T00:00:00Z", "2026-08-18T00:00:00Z");
        db.upsert_label(&make_label("l1", "bug", "default")).unwrap();
        db.replace_issue_labels(&a.id, &["l1".to_string()]).unwrap();
        db.replace_issue_labels(&b.id, &["l1".to_string()]).unwrap();

        let mut params = base_params("default");
        let wanted = vec!["l1".to_string()];
        params.label_ids = Some(&wanted);
        params.dates.updated_after = Some("2026-08-15T00:00:00Z".to_string());
        let results = db.list_issues(&params).unwrap();
        assert_eq!(ids(&results), vec!["HPN-2"]);
    }

    #[test]
    fn excludes_archived_unless_requested() {
        let (db, _dir) = test_db();
        seed(&db, "HPN-1", "HPN", "2026-08-01T00:00:00Z", "2026-08-10T00:00:00Z");
        let mut archived = make_issue("HPN-2", "HPN");
        archived.updated_at = "2026-08-15T00:00:00Z".to_string();
        archived.archived_at = Some("2026-08-16T00:00:00Z".to_string());
        db.upsert_issue(&archived).unwrap();

        let results = db.list_issues(&base_params("default")).unwrap();
        assert_eq!(ids(&results), vec!["HPN-1"]);

        let mut params = base_params("default");
        params.include_archived = true;
        let results = db.list_issues(&params).unwrap();
        assert_eq!(ids(&results), vec!["HPN-2", "HPN-1"]);
    }

    #[test]
    fn orders_by_created_when_requested() {
        let (db, _dir) = test_db();
        seed(&db, "HPN-1", "HPN", "2026-08-05T00:00:00Z", "2026-08-20T00:00:00Z");
        seed(&db, "HPN-2", "HPN", "2026-08-10T00:00:00Z", "2026-08-11T00:00:00Z");

        let mut params = base_params("default");
        params.order = ListOrder::CreatedDesc;
        let results = db.list_issues(&params).unwrap();
        assert_eq!(ids(&results), vec!["HPN-2", "HPN-1"]);
    }

    #[test]
    fn priority_order_puts_urgent_first_and_none_last() {
        let (db, _dir) = test_db();
        let mut urgent = make_issue("HPN-1", "HPN");
        urgent.priority = 1;
        db.upsert_issue(&urgent).unwrap();
        let mut none = make_issue("HPN-2", "HPN");
        none.priority = 0;
        db.upsert_issue(&none).unwrap();
        let mut medium = make_issue("HPN-3", "HPN");
        medium.priority = 3;
        db.upsert_issue(&medium).unwrap();

        let mut params = base_params("default");
        params.order = ListOrder::Priority;
        let results = db.list_issues(&params).unwrap();
        assert_eq!(ids(&results), vec!["HPN-1", "HPN-3", "HPN-2"]);
    }

    #[test]
    fn limit_and_offset_paginate() {
        let (db, _dir) = test_db();
        seed(&db, "HPN-1", "HPN", "2026-08-01T00:00:00Z", "2026-08-10T00:00:00Z");
        seed(&db, "HPN-2", "HPN", "2026-08-01T00:00:00Z", "2026-08-11T00:00:00Z");
        seed(&db, "HPN-3", "HPN", "2026-08-01T00:00:00Z", "2026-08-12T00:00:00Z");

        let mut params = base_params("default");
        params.limit = 2;
        let page1 = db.list_issues(&params).unwrap();
        assert_eq!(ids(&page1), vec!["HPN-3", "HPN-2"]);

        params.offset = 2;
        let page2 = db.list_issues(&params).unwrap();
        assert_eq!(ids(&page2), vec!["HPN-1"]);
    }

    #[test]
    fn pagination_is_stable_when_timestamps_tie() {
        let (db, _dir) = test_db();
        // Insert in REVERSE id order so insertion order (rowid) disagrees with
        // the id tie-breaker: SQLite's accidental rowid ordering would return
        // id-6..id-1 and fail the sorted assertion below.
        for n in (1..=6).rev() {
            // identical created_at AND updated_at across all six issues
            let mut issue = make_issue(&format!("HPN-{n}"), "HPN");
            issue.id = format!("id-{n}");
            issue.created_at = "2026-08-01T00:00:00Z".to_string();
            issue.updated_at = "2026-08-10T00:00:00Z".to_string();
            db.upsert_issue(&issue).unwrap();
        }

        // Contract: ties break ascending by id, not by insertion order.
        let mut params = base_params("default");
        params.limit = 100;
        let ids: Vec<String> = db.list_issues(&params).unwrap().iter().map(|i| i.id.clone()).collect();
        assert_eq!(ids, vec!["id-1", "id-2", "id-3", "id-4", "id-5", "id-6"]);

        let mut params = base_params("default");
        params.limit = 100;
        let full: Vec<String> = db.list_issues(&params).unwrap().iter().map(|i| i.identifier.clone()).collect();

        let mut paged = Vec::new();
        for offset in [0, 2, 4] {
            let mut p = base_params("default");
            p.limit = 2;
            p.offset = offset;
            paged.extend(db.list_issues(&p).unwrap().iter().map(|i| i.identifier.clone()));
        }
        assert_eq!(paged, full, "paged reads must neither duplicate nor skip tied records");

        // Same guarantee under priority ordering (all priorities equal here too).
        let mut p = base_params("default");
        p.order = ListOrder::Priority;
        p.limit = 100;
        let full_prio: Vec<String> = db.list_issues(&p).unwrap().iter().map(|i| i.identifier.clone()).collect();
        let mut paged_prio = Vec::new();
        for offset in [0, 3] {
            let mut p = base_params("default");
            p.order = ListOrder::Priority;
            p.limit = 3;
            p.offset = offset;
            paged_prio.extend(db.list_issues(&p).unwrap().iter().map(|i| i.identifier.clone()));
        }
        assert_eq!(paged_prio, full_prio);
    }
}
