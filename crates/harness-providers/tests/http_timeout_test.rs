//! A provider timeout below ten minutes must be honoured.
//!
//! `build_http_client` used to raise every configured value below 600 seconds to 600, so a provider
//! the user set to 30 seconds could hold a session — and a scheduled task — for ten minutes while a
//! stalled route said nothing.

use std::time::{Duration, Instant};

use harness_providers::build_http_client;
use tokio::net::TcpListener;

#[tokio::test]
async fn a_one_second_timeout_is_not_raised_to_ten_minutes() {
    // Accept the connection, then say nothing: only the client's own timeout can end the wait.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        tokio::time::sleep(Duration::from_secs(300)).await;
        drop(socket);
    });

    let client = build_http_client(1).expect("client");
    let started = Instant::now();
    let error = client
        .get(format!("http://{addr}/slow"))
        .send()
        .await
        .expect_err("a silent server must time out");
    let elapsed = started.elapsed();

    assert!(
        elapsed < Duration::from_secs(30),
        "a one-second timeout must fire quickly, took {elapsed:?}"
    );
    assert!(
        error.is_timeout(),
        "the failure must be the configured timeout: {error:?}"
    );

    server.abort();
}
