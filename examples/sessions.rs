//! Session tree basics: append turns, fork from an entry, persist as JSONL.
//!
//! ```bash
//! cargo run --example sessions
//! ```

use rx4::{Role, Session};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut session = Session::new("demo", "demo session");

    let first = session.append(Role::User, "explain the agent loop");
    session.append(
        Role::Assistant,
        "the loop streams a model turn, then runs tools",
    );

    // Fork the transcript at any entry id to explore an alternative branch.
    let mut branch = session.fork(first);
    branch.append(Role::User, "explain it for a host author instead");

    println!("root entries: {}", session.entries.len());
    println!("branch entries: {}", branch.entries.len());

    // Merge the branch's new entries back into the parent.
    let merged = session.merge(&branch);
    println!("merged {merged} entries");

    let dir = std::env::temp_dir().join("rx4-examples");
    std::fs::create_dir_all(&dir)?;
    let path = session.save_jsonl(&dir)?;
    let reloaded = Session::load_jsonl(&path)?;
    println!(
        "reloaded {} entries from {}",
        reloaded.entries.len(),
        path.display()
    );

    Ok(())
}
