//! A manual run and a due tick must not execute the same automation at the same time.
//!
//! `SchedulerService::run_now_cancellable` is the path the web `/api/schedules/{id}/run` route and
//! the background ticker share. It marks the task in flight before running the handler, which is
//! what makes a double click a refusal rather than a second run that overwrites the delivery
//! receipt.

use std::sync::Arc;
use std::time::Duration;

use harness_scheduler::{
    CancellableDeliveryHandler, DeliveryReceipt, DeliveryTarget, ScheduleStore, SchedulerService,
};
use tokio::sync::{Notify, Semaphore};

#[tokio::test]
async fn a_second_manual_run_of_a_live_task_is_refused_and_records_one_receipt() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Arc::new(ScheduleStore::open(Some(&tmp.path().join("schedules.db"))).unwrap());
    let task = store
        .create(
            None,
            "Nightly check".to_string(),
            "run the checks".to_string(),
            "daily:09:00".to_string(),
            DeliveryTarget::NewSessionEachRun,
            None,
        )
        .unwrap();
    let service = Arc::new(SchedulerService::new(store.clone()));

    // The handler blocks until the test releases it, so "in flight" is observable.
    let gate = Arc::new(Semaphore::new(0));
    let started = Arc::new(Notify::new());
    let handler: CancellableDeliveryHandler = {
        let gate = gate.clone();
        let started = started.clone();
        Arc::new(move |_task, _cancel| {
            let gate = gate.clone();
            let started = started.clone();
            Box::pin(async move {
                started.notify_one();
                let permit = gate
                    .acquire()
                    .await
                    .map_err(|error| format!("gate closed: {error}"))?;
                permit.forget();
                Ok(DeliveryReceipt {
                    delivered_at: chrono::Utc::now(),
                    session_id: "sess-1".to_string(),
                    status: "delivered".to_string(),
                    summary: "ran".to_string(),
                })
            })
        })
    };

    let id = task.id.clone();
    let first = {
        let service = service.clone();
        let handler = handler.clone();
        let id = id.clone();
        tokio::spawn(async move { service.run_now_cancellable(&id, &handler).await })
    };
    tokio::time::timeout(Duration::from_secs(5), started.notified())
        .await
        .expect("the first run must reach the handler");

    let second = service.run_now_cancellable(&id, &handler).await;
    let refused = second.expect_err("a live automation must refuse a second run");
    assert!(
        refused.contains("already running"),
        "the refusal names the conflict: {refused}"
    );

    gate.add_permits(1);
    first
        .await
        .expect("join")
        .expect("the accepted run completes");

    let recorded = store.get(&id).unwrap();
    assert_eq!(
        recorded.history.len(),
        1,
        "one accepted execution records exactly one receipt"
    );
    assert!(
        !service.is_running(&id),
        "the in-flight mark is cleared once the run finishes"
    );

    // A later run is allowed again.
    gate.add_permits(1);
    service
        .run_now_cancellable(&id, &handler)
        .await
        .expect("the task is runnable once it is no longer in flight");
    assert_eq!(store.get(&id).unwrap().history.len(), 2);
}
