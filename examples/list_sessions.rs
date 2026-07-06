//! Lists local Claude Code sessions.
//!
//! Usage:
//!   cargo run --example list_sessions            # all projects
//!   cargo run --example list_sessions -- <dir>   # one project directory

use std::path::Path;

use claude_agent_sdk::list_sessions;

fn main() -> Result<(), claude_agent_sdk::Error> {
    let arg = std::env::args().nth(1);
    let dir = arg.as_deref().map(Path::new);

    let sessions = list_sessions(dir, Some(20), 0, true)?;
    println!("{} session(s):", sessions.len());
    for s in sessions {
        let branch = s.git_branch.as_deref().unwrap_or("-");
        println!(
            "  {}  [{}]  {}",
            s.session_id,
            branch,
            s.summary.chars().take(70).collect::<String>()
        );
    }
    Ok(())
}
