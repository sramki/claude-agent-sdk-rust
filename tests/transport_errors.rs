//! Deterministic transport error-path tests. These use real inputs (a bogus
//! CLI path, a missing working directory) — no mocks, no live API — so they
//! run everywhere and exercise the real error mapping in
//! `SubprocessCliTransport::connect`.

use claude_agent_sdk::{ClaudeAgentOptions, Error, SubprocessCliTransport, Transport};

#[tokio::test]
async fn connect_reports_cli_not_found() {
    // Skip the version probe so the failure comes from the spawn, deterministically.
    std::env::set_var("CLAUDE_AGENT_SDK_SKIP_VERSION_CHECK", "1");

    let options = ClaudeAgentOptions {
        cli_path: Some("/definitely/not/a/real/claude-binary".into()),
        ..Default::default()
    };
    let mut transport = SubprocessCliTransport::new(options);
    let err = transport.connect().await.expect_err("spawn should fail");
    assert!(
        matches!(err, Error::CliNotFound { .. }),
        "expected CliNotFound, got {err:?}"
    );
}

#[tokio::test]
async fn connect_reports_missing_working_directory() {
    std::env::set_var("CLAUDE_AGENT_SDK_SKIP_VERSION_CHECK", "1");

    let options = ClaudeAgentOptions {
        // A resolvable executable so we get past CLI resolution to the cwd check.
        cli_path: Some("/bin/true".into()),
        cwd: Some("/no/such/working/directory".into()),
        ..Default::default()
    };
    let mut transport = SubprocessCliTransport::new(options);
    let err = transport.connect().await.expect_err("missing cwd should fail");
    match err {
        Error::Connection(msg) => assert!(
            msg.contains("Working directory does not exist"),
            "unexpected message: {msg}"
        ),
        other => panic!("expected Connection error, got {other:?}"),
    }
}
