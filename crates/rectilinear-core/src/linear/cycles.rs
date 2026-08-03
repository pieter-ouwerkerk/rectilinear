use std::future::ready;

use anyhow::Result;
use serde::Deserialize;
use uuid::Uuid;

use crate::db::{self, Database};

use super::pagination::{paginate, ConnectionPage, LinearOperation, PageInfo};
use super::LinearClient;

#[derive(Debug, Deserialize)]
struct CyclesData {
    cycles: CycleConnection,
}

#[derive(Debug, Deserialize)]
struct CycleConnection {
    nodes: Vec<LinearCycle>,
    #[serde(rename = "pageInfo")]
    page_info: PageInfo,
}

#[derive(Debug, Deserialize)]
struct LinearCycle {
    id: String,
    number: i32,
    name: Option<String>,
    #[serde(rename = "startsAt")]
    starts_at: Option<String>,
    #[serde(rename = "endsAt")]
    ends_at: Option<String>,
    #[serde(rename = "completedAt")]
    completed_at: Option<String>,
    #[serde(rename = "archivedAt")]
    archived_at: Option<String>,
    #[serde(rename = "createdAt")]
    created_at: String,
    #[serde(rename = "updatedAt")]
    updated_at: String,
    team: LinearCycleTeam,
}

#[derive(Debug, Deserialize)]
struct LinearCycleTeam {
    id: String,
    key: String,
}

impl LinearClient {
    pub async fn sync_cycles(
        &self,
        db: &Database,
        team_key: &str,
        workspace_id: &str,
        include_archived: bool,
    ) -> Result<usize> {
        let sync_token = Uuid::new_v4().to_string();
        db.mark_sync_family_running(
            workspace_id,
            team_key,
            "cycles",
            None,
            Some(self.sync_query_config.page_size(LinearOperation::Cycles)),
            &sync_token,
        )?;
        let query = r#"
            query($teamKey: String!, $first: Int!, $after: String, $includeArchived: Boolean!) {
                cycles(
                    first: $first,
                    after: $after,
                    filter: { team: { key: { eq: $teamKey } } },
                    includeArchived: $includeArchived,
                    orderBy: updatedAt
                ) {
                    nodes {
                        id number name startsAt endsAt completedAt archivedAt createdAt updatedAt
                        team { id key }
                    }
                    pageInfo { hasNextPage endCursor }
                }
            }
        "#;
        let result = paginate(
            &self.sync_query_config,
            LinearOperation::Cycles,
            Some(team_key.to_string()),
            |request| async move {
                let data: CyclesData = self
                    .query_operation(
                        LinearOperation::Cycles.name(),
                        request.cursor.as_deref(),
                        query,
                        serde_json::json!({
                            "teamKey": team_key,
                            "first": request.page_size,
                            "after": request.cursor,
                            "includeArchived": include_archived,
                        }),
                    )
                    .await?;
                Ok(ConnectionPage {
                    nodes: data.cycles.nodes,
                    page_info: data.cycles.page_info,
                })
            },
            |nodes, _| {
                let result = nodes.into_iter().try_for_each(|cycle| {
                    db.upsert_cycle(
                        &db::Cycle {
                            id: cycle.id,
                            workspace_id: workspace_id.to_string(),
                            team_id: cycle.team.id,
                            team_key: cycle.team.key,
                            number: cycle.number,
                            name: cycle.name,
                            starts_at: cycle.starts_at,
                            ends_at: cycle.ends_at,
                            completed_at: cycle.completed_at,
                            archived_at: cycle.archived_at,
                            created_at: cycle.created_at,
                            updated_at: cycle.updated_at,
                        },
                        &sync_token,
                    )
                });
                ready(result)
            },
            |cycle| cycle.id.clone(),
            |event| self.observe_sync_event(event),
        )
        .await;

        match result {
            Ok(stats) => {
                db.reconcile_cycles(workspace_id, team_key, &sync_token)?;
                db.mark_sync_family_complete(
                    workspace_id,
                    team_key,
                    "cycles",
                    Some(self.sync_query_config.page_size(LinearOperation::Cycles)),
                    &sync_token,
                )?;
                Ok(stats.nodes)
            }
            Err(error) => {
                let message = self.redacted_error_message(&error);
                db.mark_sync_family_failed(
                    workspace_id,
                    team_key,
                    "cycles",
                    &sync_token,
                    &message,
                )?;
                Err(error)
            }
        }
    }
}
