// Run the actual hook commands in a separate process so NOTMUX_CONFIG_DIR
// never races other tests or touches the user's configuration.
#[test]
fn isolated_hook_command() {
    let Ok(command) = std::env::var("NOTMUX_TEST_HOOK_COMMAND") else {
        return;
    };
    assert!(std::env::var_os("NOTMUX_CONFIG_DIR").is_some());
    let status = match command.as_str() {
        "working" => super::commands::cli_agent_status(&[
            "working".into(), "--terminal".into(), "hook-test".into(),
        ]),
        "stop" => super::commands::cli_notify(&[
            "--title".into(), "Claude Code".into(),
            "--body".into(), "Turn complete".into(),
            "--terminal-id".into(), "hook-test".into(),
        ]),
        other => panic!("unexpected hook command: {other}"),
    };
    let expected = i32::from(std::env::var("NOTMUX_TEST_EXPECT_FAILURE").as_deref() == Ok("1"));
    assert_eq!(status, expected, "unexpected hook result");
}

#[tokio::test]
async fn local_cli_client_does_not_follow_redirects() {
    use axum::{Router, routing::post, response::Redirect};
    let router = Router::new().route("/", post(|| async {
        Redirect::temporary("http://127.0.0.1:1/credential-leak")
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, router.into_make_service()).await.unwrap();
    });
    let status = tokio::task::spawn_blocking(move || {
        super::local_http_client().unwrap().post(url)
            .bearer_auth("dummy-local-proof")
            .timeout(std::time::Duration::from_secs(2))
            .send().unwrap().status()
    }).await.unwrap();
    assert_eq!(status, reqwest::StatusCode::TEMPORARY_REDIRECT);
    server.abort();
}
