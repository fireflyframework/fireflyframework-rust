// Copyright 2026 Firefly Software Foundation.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! End-to-end test for `#[firefly::aspect]`: the macro expands against the real
//! `firefly` facade, the generated `inventory` thunk registers the aspect into
//! the process-global registry on discovery, and weaving a call through
//! `firefly::advised` runs the declared `#[before]` / `#[around]` advice.

use std::sync::{Arc, Mutex};

use firefly::prelude::*;

type Log = Arc<Mutex<Vec<String>>>;

// A shared sink the aspect writes its observed advice into. The aspect is
// constructed via `Default` (the macro's discovery thunk requires it), so it
// finds the sink through a process-global handle rather than a constructor
// argument — the honest Rust mirror of a Spring singleton aspect reading a
// shared collaborator.
fn sink() -> Log {
    static SINK: Mutex<Option<Log>> = Mutex::new(None);
    let mut guard = SINK.lock().unwrap();
    guard
        .get_or_insert_with(|| Arc::new(Mutex::new(Vec::new())))
        .clone()
}

#[derive(Default)]
struct AuditAspect;

#[firefly::aspect(pointcut = "svc.Audited.*", order = 0)]
impl AuditAspect {
    // A plain, still-callable method (the marker is stripped, not the method).
    #[before]
    async fn record_entry(&self, jp: &JoinPoint) {
        sink()
            .lock()
            .unwrap()
            .push(format!("before:{}", jp.qualified_name()));
    }

    #[around]
    fn time_it<'a>(&'a self, _jp: &'a JoinPoint, proceed: Proceed<'a>) -> AdviceFuture<'a> {
        Box::pin(async move {
            sink().lock().unwrap().push("around:before".to_string());
            let result = proceed.proceed().await;
            sink().lock().unwrap().push("around:after".to_string());
            result
        })
    }
}

// The three scenarios share one process-global sink and the process-global
// aspect registry, so they run as ONE test (cargo runs test functions in
// parallel; splitting them would race on the shared sink's clear/assert).
#[tokio::test]
async fn aspect_macro_discovers_weaves_and_keeps_methods_callable() {
    let log = sink();

    // Discovery drains the macro's inventory thunk and registers the aspect.
    register_discovered_aspects();

    // (1) A matching call: the declared before + around advice fire.
    log.lock().unwrap().clear();
    let out = advised("svc.Audited", "create", Arc::new((1u32,)), || async {
        ok("order-1".to_string())
    })
    .await
    .unwrap();
    assert_eq!(out.downcast_ref::<String>().unwrap(), "order-1");
    assert_eq!(
        *log.lock().unwrap(),
        vec!["before:svc.Audited.create", "around:before", "around:after"]
    );

    // (2) A non-matching qualified name: the call runs with no advice.
    log.lock().unwrap().clear();
    let out = advised("svc.Other", "create", Arc::new(()), || async {
        ok("plain".to_string())
    })
    .await
    .unwrap();
    assert_eq!(out.downcast_ref::<String>().unwrap(), "plain");
    assert!(
        log.lock().unwrap().is_empty(),
        "a non-matching call must not be advised"
    );

    // (3) The macro strips only the markers, so `record_entry` remains an
    // ordinary, directly-callable method.
    log.lock().unwrap().clear();
    let jp = JoinPoint::new("svc.Audited", "direct", Arc::new(()));
    AuditAspect.record_entry(&jp).await;
    assert_eq!(*log.lock().unwrap(), vec!["before:svc.Audited.direct"]);
}
