// Integration tests for the declarative `#[firefly::workflow]` and
// `#[firefly::tcc]` macros: DAG nodes with parallel layers + dependencies, and
// Try/Confirm/Cancel participants with injected, published-and-read state.

use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
struct Amount(i64);

#[derive(Debug)]
struct DemoError(&'static str);
impl std::fmt::Display for DemoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}
impl std::error::Error for DemoError {}

type Log = Arc<Mutex<Vec<String>>>;

// ---- #[workflow] ----------------------------------------------------------

struct Compliance {
    log: Log,
}

#[firefly::workflow(name = "compliance")]
impl Compliance {
    #[workflow_step(id = "balance-check")]
    async fn balance(&self, #[input] amt: Amount) -> Result<bool, DemoError> {
        self.log.lock().unwrap().push("balance".into());
        Ok(amt.0 <= 1000)
    }

    #[workflow_step(id = "fraud-scan")]
    async fn fraud(&self, #[input] amt: Amount) -> Result<bool, DemoError> {
        self.log.lock().unwrap().push("fraud".into());
        Ok(amt.0 != 666)
    }

    #[workflow_step(id = "approve", depends_on = ["balance-check", "fraud-scan"])]
    async fn approve(
        &self,
        #[from_step("balance-check")] within_limit: bool,
    ) -> Result<(), DemoError> {
        self.log
            .lock()
            .unwrap()
            .push(format!("approve {within_limit}"));
        if within_limit {
            Ok(())
        } else {
            Err(DemoError("over limit"))
        }
    }
}

#[tokio::test]
async fn workflow_macro_runs_dag_with_parallel_layer() {
    let log: Log = Arc::new(Mutex::new(Vec::new()));
    let wf = Arc::new(Compliance { log: log.clone() });
    wf.run(Amount(500)).await.expect("workflow approves");
    let trace = log.lock().unwrap().clone();
    assert!(trace.iter().any(|x| x == "balance"), "{trace:?}");
    assert!(trace.iter().any(|x| x == "fraud"), "{trace:?}");
    // approve depends on both checks, so it must come last.
    let approve_at = trace.iter().position(|x| x.starts_with("approve")).unwrap();
    assert_eq!(
        approve_at,
        trace.len() - 1,
        "approve runs after both checks: {trace:?}"
    );
}

// ---- #[tcc] ---------------------------------------------------------------

struct Transfer2pc {
    log: Log,
    fail_dest_try: bool,
}

#[firefly::tcc(name = "transfer-2pc")]
impl Transfer2pc {
    #[participant(name = "source", confirm = "capture_source", cancel = "release_source")]
    async fn hold_source(&self, #[input] amt: Amount) -> Result<i64, DemoError> {
        self.log.lock().unwrap().push("try-source".into());
        Ok(amt.0)
    }
    async fn capture_source(&self, #[from_step("source")] held: i64) -> Result<(), DemoError> {
        self.log
            .lock()
            .unwrap()
            .push(format!("confirm-source {held}"));
        Ok(())
    }
    async fn release_source(&self, #[from_step("source")] held: i64) -> Result<(), DemoError> {
        self.log
            .lock()
            .unwrap()
            .push(format!("cancel-source {held}"));
        Ok(())
    }

    #[participant(name = "dest", confirm = "capture_dest")]
    async fn hold_dest(&self, #[input] _amt: Amount) -> Result<(), DemoError> {
        self.log.lock().unwrap().push("try-dest".into());
        if self.fail_dest_try {
            return Err(DemoError("destination unavailable"));
        }
        Ok(())
    }
    async fn capture_dest(&self) -> Result<(), DemoError> {
        self.log.lock().unwrap().push("confirm-dest".into());
        Ok(())
    }
}

#[tokio::test]
async fn tcc_macro_confirms_all_on_success() {
    let log: Log = Arc::new(Mutex::new(Vec::new()));
    let tcc = Arc::new(Transfer2pc {
        log: log.clone(),
        fail_dest_try: false,
    });
    tcc.run(Amount(100)).await.expect("tcc confirms");
    let trace = log.lock().unwrap().clone();
    assert!(
        trace.contains(&"confirm-source 100".to_string()),
        "{trace:?}"
    );
    assert!(trace.contains(&"confirm-dest".to_string()), "{trace:?}");
}

#[tokio::test]
async fn tcc_macro_cancels_tried_on_try_failure() {
    let log: Log = Arc::new(Mutex::new(Vec::new()));
    let tcc = Arc::new(Transfer2pc {
        log: log.clone(),
        fail_dest_try: true,
    });
    tcc.run(Amount(100)).await.expect_err("dest try fails");
    let trace = log.lock().unwrap().clone();
    // source was tried first, so it must be cancelled when dest's try fails.
    assert!(
        trace.contains(&"cancel-source 100".to_string()),
        "{trace:?}"
    );
    assert!(
        !trace.iter().any(|x| x.starts_with("confirm")),
        "nothing confirmed: {trace:?}"
    );
}
