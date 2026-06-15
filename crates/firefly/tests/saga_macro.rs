// Integration test for the declarative `#[firefly::saga]` macro: argument
// injection (`#[input]` / `#[from_step]`), `depends_on` ordering, result
// passing, and compensation on failure — all through the one `firefly` facade.

use std::sync::{Arc, Mutex};

use firefly::orchestration::SagaStatus;
use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
struct Transfer {
    amount: i64,
}

#[derive(Clone, Serialize, Deserialize)]
struct Reserved {
    hold: i64,
}

#[derive(Debug)]
struct DemoError(&'static str);
impl std::fmt::Display for DemoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}
impl std::error::Error for DemoError {}

type Log = Arc<Mutex<Vec<String>>>;

struct TransferSaga {
    log: Log,
    fail_credit: bool,
}

#[firefly::saga(name = "demo-transfer", policy = "stop_on_error")]
impl TransferSaga {
    #[saga_step(id = "reserve", compensate = "release")]
    async fn reserve(&self, #[input] t: Transfer) -> Result<Reserved, DemoError> {
        self.log.lock().unwrap().push("reserve".into());
        Ok(Reserved { hold: t.amount })
    }

    // Compensation for `reserve`; reads the reserve result via #[from_step].
    async fn release(&self, #[from_step("reserve")] r: Reserved) -> Result<(), DemoError> {
        self.log.lock().unwrap().push(format!("release {}", r.hold));
        Ok(())
    }

    #[saga_step(id = "credit", depends_on = ["reserve"])]
    async fn credit(&self, #[from_step("reserve")] r: Reserved) -> Result<(), DemoError> {
        if self.fail_credit {
            self.log.lock().unwrap().push("credit-failed".into());
            return Err(DemoError("credit rejected"));
        }
        self.log.lock().unwrap().push(format!("credit {}", r.hold));
        Ok(())
    }
}

#[tokio::test]
async fn saga_macro_runs_in_dependency_order_with_injection() {
    let log: Log = Arc::new(Mutex::new(Vec::new()));
    let saga = Arc::new(TransferSaga {
        log: log.clone(),
        fail_credit: false,
    });
    let outcome = saga
        .run(Transfer { amount: 100 })
        .await
        .expect("saga completes");
    assert_eq!(outcome.status, SagaStatus::Completed);
    assert_eq!(outcome.steps_executed, vec!["reserve", "credit"]);
    assert_eq!(*log.lock().unwrap(), vec!["reserve", "credit 100"]);
}

#[tokio::test]
async fn saga_macro_compensates_on_step_failure() {
    let log: Log = Arc::new(Mutex::new(Vec::new()));
    let saga = Arc::new(TransferSaga {
        log: log.clone(),
        fail_credit: true,
    });
    let failure = saga
        .run(Transfer { amount: 50 })
        .await
        .expect_err("credit fails");
    assert_eq!(failure.outcome().status, SagaStatus::Compensated);
    let trace = log.lock().unwrap().clone();
    assert!(
        trace.contains(&"release 50".to_string()),
        "reserve must be compensated after credit fails: {trace:?}"
    );
}
