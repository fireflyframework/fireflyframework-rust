//! `GET /actuator/scheduledtasks` — Spring Boot parity via a local
//! source trait so the actuator stays decoupled from
//! `firefly-scheduling` (the starter bridges the two).

use std::time::Duration;

use serde_json::{json, Value};

/// Trigger kind of a scheduled task, mirroring Spring's
/// `/actuator/scheduledtasks` grouping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskTrigger {
    /// A cron-triggered task.
    Cron {
        /// The cron expression driving the task.
        expression: String,
    },
    /// A fixed-rate task (next run is scheduled from the previous start).
    FixedRate {
        /// Interval between runs.
        interval: Duration,
        /// Optional delay before the first run.
        initial_delay: Option<Duration>,
    },
    /// A fixed-delay task (next run is scheduled from the previous end).
    FixedDelay {
        /// Interval between the end of one run and the start of the next.
        interval: Duration,
        /// Optional delay before the first run.
        initial_delay: Option<Duration>,
    },
}

/// One scheduled task as reported on `/actuator/scheduledtasks` —
/// pyfly's `@scheduled` metadata flattened into a descriptor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskDescriptor {
    /// The runnable target, e.g. `ReportService.emit`.
    pub name: String,
    /// How the task is triggered.
    pub trigger: TaskTrigger,
}

/// Supplies the scheduled-task descriptors served on
/// `GET /actuator/scheduledtasks`. Implemented by the scheduling
/// integration (e.g. a starter bridging `firefly-scheduling`) so this
/// crate carries no scheduler dependency.
pub trait ScheduledTasksSource: Send + Sync {
    /// Snapshot of the currently registered scheduled tasks.
    fn scheduled_tasks(&self) -> Vec<TaskDescriptor>;
}

/// A [`ScheduledTasksSource`] over a fixed descriptor list — convenient
/// for apps that declare their schedule statically (and for tests).
pub struct StaticScheduledTasks(pub Vec<TaskDescriptor>);

impl ScheduledTasksSource for StaticScheduledTasks {
    fn scheduled_tasks(&self) -> Vec<TaskDescriptor> {
        self.0.clone()
    }
}

/// Renders Spring's wire shape: `{"cron": […], "fixedDelay": […],
/// "fixedRate": […]}` with intervals in milliseconds.
pub(crate) fn render_tasks(tasks: &[TaskDescriptor]) -> Value {
    let mut cron = Vec::new();
    let mut fixed_delay = Vec::new();
    let mut fixed_rate = Vec::new();

    for task in tasks {
        let runnable = json!({ "target": task.name });
        match &task.trigger {
            TaskTrigger::Cron { expression } => {
                cron.push(json!({ "runnable": runnable, "expression": expression }));
            }
            TaskTrigger::FixedRate {
                interval,
                initial_delay,
            } => {
                fixed_rate.push(json!({
                    "runnable": runnable,
                    "interval": interval.as_millis() as u64,
                    "initialDelay": initial_delay.map(|d| d.as_millis() as u64),
                }));
            }
            TaskTrigger::FixedDelay {
                interval,
                initial_delay,
            } => {
                fixed_delay.push(json!({
                    "runnable": runnable,
                    "interval": interval.as_millis() as u64,
                    "initialDelay": initial_delay.map(|d| d.as_millis() as u64),
                }));
            }
        }
    }

    json!({ "cron": cron, "fixedDelay": fixed_delay, "fixedRate": fixed_rate })
}

#[cfg(test)]
mod tests {
    use super::*;

    // pyfly: test_scheduledtasks_groups_by_trigger
    #[test]
    fn groups_by_trigger_with_millis() {
        let tasks = vec![
            TaskDescriptor {
                name: "ReportService.emit".into(),
                trigger: TaskTrigger::FixedRate {
                    interval: Duration::from_secs(30),
                    initial_delay: None,
                },
            },
            TaskDescriptor {
                name: "Cleaner.purge".into(),
                trigger: TaskTrigger::Cron {
                    expression: "0 0 * * *".into(),
                },
            },
            TaskDescriptor {
                name: "Sync.pull".into(),
                trigger: TaskTrigger::FixedDelay {
                    interval: Duration::from_millis(1500),
                    initial_delay: Some(Duration::from_secs(5)),
                },
            },
        ];
        let body = render_tasks(&tasks);
        assert_eq!(
            body["fixedRate"][0]["runnable"]["target"],
            "ReportService.emit"
        );
        assert_eq!(body["fixedRate"][0]["interval"], 30000);
        assert_eq!(body["fixedRate"][0]["initialDelay"], Value::Null);
        assert_eq!(body["cron"][0]["expression"], "0 0 * * *");
        assert_eq!(body["fixedDelay"][0]["interval"], 1500);
        assert_eq!(body["fixedDelay"][0]["initialDelay"], 5000);
    }

    #[test]
    fn empty_source_renders_empty_groups() {
        let body = render_tasks(&[]);
        assert_eq!(
            body,
            serde_json::json!({"cron": [], "fixedDelay": [], "fixedRate": []})
        );
    }

    #[test]
    fn static_source_returns_snapshot() {
        let src = StaticScheduledTasks(vec![TaskDescriptor {
            name: "A.b".into(),
            trigger: TaskTrigger::Cron {
                expression: "* * * * *".into(),
            },
        }]);
        assert_eq!(src.scheduled_tasks().len(), 1);
    }
}
