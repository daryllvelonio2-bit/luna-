use super::*;

#[tokio::test]
async fn channel_approver_honors_allow_all_and_plan_mode() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let allow_all = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let plan_mode = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let approver = ChannelApprover::new(tx, allow_all.clone(), plan_mode.clone());

    let req = harness_core::ApprovalRequest::default();
    assert!(approver.approve(&req).await.is_err());

    plan_mode.store(false, std::sync::atomic::Ordering::SeqCst);
    tokio::spawn(async move {
        if let Some((_r, reply)) = rx.recv().await {
            let _ = reply.send(harness_core::ApprovalDecision::Run);
        }
    });
    assert_eq!(
        approver.approve(&req).await.unwrap(),
        harness_core::ApprovalDecision::Run
    );

    allow_all.store(true, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(
        approver.approve(&req).await.unwrap(),
        harness_core::ApprovalDecision::Run
    );
}
