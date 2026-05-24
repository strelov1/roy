//! Pure per-tick decision. Mirrors claude-agent/lib/scheduler-plan.ts.
//!
//! Caller supplies due rows (already filtered to `paused = 0` and
//! `next_fire_at <= now`), the current clock, and a cron→next-time
//! closure. Returns a `TickPlan` of mutations the driver must apply.

use chrono::{DateTime, Utc};

use crate::types::Trigger;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdvanceOp {
    pub id: String,
    pub next_fire_at: DateTime<Utc>,
    pub last_fire_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PauseOp {
    pub id: String,
    pub last_error: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TickPlan {
    /// One-shot ids to delete + fire.
    pub to_delete: Vec<String>,
    /// Recurring rows whose `next_fire_at` advances.
    pub to_advance: Vec<AdvanceOp>,
    /// Triggers with unparseable cron — paused so they leave the due set.
    pub to_pause: Vec<PauseOp>,
    /// Rows the driver dispatches through Fire after the claim txn commits.
    pub to_fire: Vec<Trigger>,
}

/// Decide the mutations for one polling tick.
///
/// Rules:
/// - `kind = 'oneshot'`  → delete + fire.
/// - `kind = 'cron'`, valid expression → advance next_fire_at + fire.
/// - `kind = 'cron'`, bad cron → pause (no fire) so it leaves the due set.
pub fn plan_tick<F>(rows: &[Trigger], now: DateTime<Utc>, compute_next: F) -> TickPlan
where
    F: Fn(&str, &str) -> Option<DateTime<Utc>>, // (cron_expr, tz) -> next
{
    let mut plan = TickPlan::default();

    for row in rows {
        if row.is_oneshot() {
            plan.to_delete.push(row.id.clone());
            plan.to_fire.push(row.clone());
            continue;
        }

        // Cron path.
        let expr = match &row.cron_expr {
            Some(e) => e.as_str(),
            None => {
                plan.to_pause.push(PauseOp {
                    id: row.id.clone(),
                    last_error: "cron trigger without expression".into(),
                });
                continue;
            }
        };
        let next = compute_next(expr, &row.timezone);
        if let Some(next_at) = next {
            plan.to_advance.push(AdvanceOp {
                id: row.id.clone(),
                next_fire_at: next_at,
                last_fire_at: now,
            });
            plan.to_fire.push(row.clone());
        } else {
            plan.to_pause.push(PauseOp {
                id: row.id.clone(),
                last_error: "invalid cron".into(),
            });
        }
    }

    plan
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn cron_trigger(id: &str, expr: &str) -> Trigger {
        Trigger {
            id: id.into(),
            agent_id: "agent-1".into(),
            kind: "cron".into(),
            cron_expr: Some(expr.into()),
            timezone: "UTC".into(),
            fire_at: None,
            next_fire_at: Utc::now(),
            last_fire_at: None,
            paused: 0,
            last_error: None,
            created_at: Utc::now(),
        }
    }

    fn oneshot_trigger(id: &str) -> Trigger {
        Trigger {
            id: id.into(),
            agent_id: "agent-1".into(),
            kind: "oneshot".into(),
            cron_expr: None,
            timezone: "UTC".into(),
            fire_at: Some(Utc::now()),
            next_fire_at: Utc::now(),
            last_fire_at: None,
            paused: 0,
            last_error: None,
            created_at: Utc::now(),
        }
    }

    fn never(_: &str, _: &str) -> Option<DateTime<Utc>> {
        None
    }

    fn always_in(seconds: i64) -> impl Fn(&str, &str) -> Option<DateTime<Utc>> {
        move |_, _| Some(Utc::now() + Duration::seconds(seconds))
    }

    #[test]
    fn oneshot_is_deleted_and_fired() {
        let rows = vec![oneshot_trigger("o1")];
        let plan = plan_tick(&rows, Utc::now(), never);
        assert_eq!(plan.to_delete, vec!["o1".to_string()]);
        assert_eq!(plan.to_fire.len(), 1);
        assert!(plan.to_advance.is_empty());
        assert!(plan.to_pause.is_empty());
    }

    #[test]
    fn cron_with_valid_expression_advances_and_fires() {
        let rows = vec![cron_trigger("c1", "*/5 * * * *")];
        let plan = plan_tick(&rows, Utc::now(), always_in(300));
        assert!(plan.to_delete.is_empty());
        assert_eq!(plan.to_advance.len(), 1);
        assert_eq!(plan.to_advance[0].id, "c1");
        assert_eq!(plan.to_fire.len(), 1);
        assert!(plan.to_pause.is_empty());
    }

    #[test]
    fn cron_with_unparseable_expression_paused_not_fired() {
        let rows = vec![cron_trigger("c2", "garbage")];
        let plan = plan_tick(&rows, Utc::now(), never);
        assert!(plan.to_fire.is_empty());
        assert_eq!(plan.to_pause.len(), 1);
        assert_eq!(plan.to_pause[0].id, "c2");
        assert_eq!(plan.to_pause[0].last_error, "invalid cron");
    }

    #[test]
    fn cron_without_expression_paused() {
        let mut row = cron_trigger("c3", "");
        row.cron_expr = None;
        let plan = plan_tick(&[row], Utc::now(), always_in(60));
        assert!(plan.to_fire.is_empty());
        assert_eq!(plan.to_pause.len(), 1);
        assert!(plan.to_pause[0].last_error.contains("without expression"));
    }

    #[test]
    fn mixed_batch_partitions_correctly() {
        let rows = vec![
            oneshot_trigger("o1"),
            cron_trigger("c1", "*/5 * * * *"),
            cron_trigger("c-bad", "huh"),
        ];
        // compute_next: only succeed for "*/5 * * * *".
        let plan = plan_tick(&rows, Utc::now(), |expr, _| {
            if expr == "*/5 * * * *" {
                Some(Utc::now() + Duration::minutes(5))
            } else {
                None
            }
        });
        assert_eq!(plan.to_delete, vec!["o1".to_string()]);
        assert_eq!(plan.to_advance.len(), 1);
        assert_eq!(plan.to_pause.len(), 1);
        assert_eq!(
            plan.to_fire.len(),
            2,
            "oneshot + valid cron fire; bad cron paused"
        );
    }
}
