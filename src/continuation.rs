//! Domain-independent continuation advice from trusted host receipts. This does
//! not execute, replay, authorize, or renew work. Experiences enforce the result.
use crate::task_bounds::Bound;
use anyhow::{Result, ensure};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Input<Time = u64, Turns = u32> {
    pub status: String,
    pub remaining_ms: Time,
    pub remaining_turns: Turns,
    pub minimum_turn_ms: u64,
    pub idle: bool,
    pub observation_current: bool,
    pub receipts_complete: bool,
    pub actions_succeeded: bool,
    pub recovery_attempts: u32,
}

pub fn assess(i: Input) -> Result<Value> {
    assess_bounds(
        Input {
            status: i.status,
            remaining_ms: Bound::Finite {
                value: i.remaining_ms,
            },
            remaining_turns: Bound::Finite {
                value: i.remaining_turns,
            },
            minimum_turn_ms: i.minimum_turn_ms,
            idle: i.idle,
            observation_current: i.observation_current,
            receipts_complete: i.receipts_complete,
            actions_succeeded: i.actions_succeeded,
            recovery_attempts: i.recovery_attempts,
        },
        "bounded-continuation/1",
    )
}

pub fn assess_v2(i: Input<Bound<u64>, Bound<u32>>) -> Result<Value> {
    assess_bounds(i, "bounded-continuation/2")
}

fn assess_bounds(i: Input<Bound<u64>, Bound<u32>>, policy: &str) -> Result<Value> {
    ensure!(
        (1000..=600_000).contains(&i.minimum_turn_ms),
        "Invalid minimum turn duration"
    );
    let (action, reason) = if !["running", "waiting", "needs_review"].contains(&i.status.as_str()) {
        ("hold", "task_not_active")
    } else if !i.remaining_turns.permits_at_least(1)
        || !i.remaining_ms.permits_at_least(i.minimum_turn_ms)
    {
        ("checkpoint", "insufficient_budget")
    } else if !i.idle || !i.observation_current {
        ("hold", "observation_not_settled")
    } else if i.status == "needs_review" {
        if i.receipts_complete && i.actions_succeeded && i.recovery_attempts < 2 {
            ("inspect_then_continue", "settled_receipts")
        } else {
            ("review", "uncertain_effects_or_recovery_limit")
        }
    } else if i.status == "waiting" {
        ("hold", "waiting_for_evidence")
    } else {
        ("continue", "active_budget")
    };
    Ok(json!({"policy":policy,"action":action,"reason":reason,
        "authority":"existing_task_only","replay_actions":false,"extend_budget":false}))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> Input {
        Input {
            status: "needs_review".into(),
            remaining_ms: 200_000,
            remaining_turns: 4,
            minimum_turn_ms: 120_000,
            idle: true,
            observation_current: true,
            receipts_complete: true,
            actions_succeeded: true,
            recovery_attempts: 0,
        }
    }
    #[test]
    fn terminal_tasks_and_limits_always_win_over_recovery() {
        for status in [
            "stopped",
            "cancelled",
            "review",
            "blocked",
            "budget_exhausted",
        ] {
            let mut i = fixture();
            i.status = status.into();
            assert_eq!(assess(i).unwrap()["action"], "hold");
        }
        let mut i = fixture();
        i.remaining_ms = 119_999;
        assert_eq!(assess(i).unwrap()["action"], "checkpoint");
        let mut i = fixture();
        i.remaining_turns = 0;
        assert_eq!(assess(i).unwrap()["action"], "checkpoint");
    }
    #[test]
    fn only_settled_observed_receipts_allow_bounded_inspection() {
        assert_eq!(
            assess(fixture()).unwrap()["action"],
            "inspect_then_continue"
        );
        for failure in 0..5 {
            let mut i = fixture();
            match failure {
                0 => i.idle = false,
                1 => i.observation_current = false,
                2 => i.receipts_complete = false,
                3 => i.actions_succeeded = false,
                _ => i.recovery_attempts = 2,
            }
            assert_ne!(assess(i).unwrap()["action"], "inspect_then_continue");
        }
        let mut i = fixture();
        i.status = "waiting".into();
        assert_eq!(assess(i).unwrap()["reason"], "waiting_for_evidence");
    }
}
