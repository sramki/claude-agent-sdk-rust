//! Reusable conformance suite for [`SessionStore`] adapters.
//!
//! Port of `testing/session_store_conformance.py`. Call
//! [`run_session_store_conformance`] from an async test to assert the behavioral
//! contracts every adapter must satisfy. Contracts for the optional methods
//! (`list_sessions`, `list_session_summaries`, `delete`, `list_subkeys`) are
//! skipped when named in `skip_optional` or when the store returns
//! [`Error::Unsupported`] from that method.
//!
//! ```no_run
//! # async fn ex() {
//! use claude_agent_sdk_rs::testing::run_session_store_conformance;
//! use claude_agent_sdk_rs::InMemorySessionStore;
//! run_session_store_conformance(InMemorySessionStore::new, &[]).await;
//! # }
//! ```

use serde_json::{json, Map, Value};

use crate::error::Error;
use crate::store::fold_session_summary;
use crate::types::{SessionKey, SessionListSubkeysKey, SessionStore, SessionStoreEntry};

fn key(project_key: &str, session_id: &str, subpath: Option<&str>) -> SessionKey {
    SessionKey {
        project_key: project_key.to_string(),
        session_id: session_id.to_string(),
        subpath: subpath.map(str::to_string),
    }
}

/// Builds a test entry (`{"type": "x", ...extra}`). `type` is required; its
/// value is irrelevant to the contracts. Mirrors the upstream `_e`.
fn entry(extra: Value) -> SessionStoreEntry {
    let mut m = Map::new();
    m.insert("type".into(), Value::String("x".into()));
    if let Value::Object(o) = extra {
        for (k, v) in o {
            m.insert(k, v);
        }
    }
    m
}

const OPTIONAL_METHODS: [&str; 4] = [
    "list_sessions",
    "list_session_summaries",
    "delete",
    "list_subkeys",
];

fn is_unsupported<T>(r: &Result<T, Error>) -> bool {
    matches!(r, Err(Error::Unsupported(_)))
}

async fn supports(store: &dyn SessionStore, method: &str, skip: &[&str]) -> bool {
    if skip.contains(&method) {
        return false;
    }
    match method {
        "list_sessions" => !is_unsupported(&store.list_sessions("__probe__").await),
        "list_session_summaries" => {
            !is_unsupported(&store.list_session_summaries("__probe__").await)
        }
        "delete" => !is_unsupported(&store.delete(&key("__probe__", "__p__", None)).await),
        "list_subkeys" => !is_unsupported(
            &store
                .list_subkeys(&SessionListSubkeysKey {
                    project_key: "__probe__".into(),
                    session_id: "__p__".into(),
                })
                .await,
        ),
        _ => false,
    }
}

fn ids(sessions: &[crate::types::SessionStoreListEntry]) -> Vec<String> {
    let mut v: Vec<String> = sessions.iter().map(|s| s.session_id.clone()).collect();
    v.sort();
    v
}

/// Asserts the [`SessionStore`] behavioral contracts. `make_store` is invoked
/// once per contract for isolation. Panics (like an assertion) on any violation.
pub async fn run_session_store_conformance<S, F>(make_store: F, skip_optional: &[&str])
where
    S: SessionStore,
    F: Fn() -> S,
{
    for m in skip_optional {
        assert!(
            OPTIONAL_METHODS.contains(m),
            "unknown optional method in skip_optional: {m}"
        );
    }

    let k = key("proj", "sess", None);
    let probe = make_store();
    let has_list_sessions = supports(&probe, "list_sessions", skip_optional).await;
    let has_list_summaries = supports(&probe, "list_session_summaries", skip_optional).await;
    let has_delete = supports(&probe, "delete", skip_optional).await;
    let has_list_subkeys = supports(&probe, "list_subkeys", skip_optional).await;

    // 1. append then load returns the same entries in the same order.
    let store = make_store();
    store
        .append(&k, &[entry(json_n("b", 1)), entry(json_n("a", 2))])
        .await
        .unwrap();
    assert_eq!(
        store.load(&k).await.unwrap(),
        Some(vec![entry(json_n("b", 1)), entry(json_n("a", 2))])
    );

    // 2. load of an unknown key / subpath returns None.
    let store = make_store();
    assert_eq!(store.load(&key("proj", "nope", None)).await.unwrap(), None);
    store.append(&k, &[entry(json_n("x", 1))]).await.unwrap();
    assert_eq!(store.load(&key("proj", "sess", Some("nope"))).await.unwrap(), None);

    // 3. multiple append calls preserve call order.
    let store = make_store();
    store.append(&k, &[entry(json_n("z", 1))]).await.unwrap();
    store
        .append(&k, &[entry(json_n("a", 2)), entry(json_n("m", 3))])
        .await
        .unwrap();
    store.append(&k, &[entry(json_n("b", 4))]).await.unwrap();
    assert_eq!(
        store.load(&k).await.unwrap(),
        Some(vec![
            entry(json_n("z", 1)),
            entry(json_n("a", 2)),
            entry(json_n("m", 3)),
            entry(json_n("b", 4)),
        ])
    );

    // 4. append([]) is a no-op.
    let store = make_store();
    store.append(&k, &[entry(json_n("a", 1))]).await.unwrap();
    store.append(&k, &[]).await.unwrap();
    assert_eq!(store.load(&k).await.unwrap(), Some(vec![entry(json_n("a", 1))]));

    // 5. subpath keys are stored independently of the main transcript.
    let store = make_store();
    let sub = key("proj", "sess", Some("subagents/agent-1"));
    store.append(&k, &[entry(json_n("m", 1))]).await.unwrap();
    store.append(&sub, &[entry(json_n("s", 1))]).await.unwrap();
    assert_eq!(store.load(&k).await.unwrap(), Some(vec![entry(json_n("m", 1))]));
    assert_eq!(store.load(&sub).await.unwrap(), Some(vec![entry(json_n("s", 1))]));

    // 6. project_key isolation.
    let store = make_store();
    store.append(&key("A", "s1", None), &[entry(json!({"from": "A"}))]).await.unwrap();
    store.append(&key("B", "s1", None), &[entry(json!({"from": "B"}))]).await.unwrap();
    assert_eq!(
        store.load(&key("A", "s1", None)).await.unwrap(),
        Some(vec![entry(json!({"from": "A"}))])
    );
    assert_eq!(
        store.load(&key("B", "s1", None)).await.unwrap(),
        Some(vec![entry(json!({"from": "B"}))])
    );
    if has_list_sessions {
        assert_eq!(store.list_sessions("A").await.unwrap().len(), 1);
        assert_eq!(store.list_sessions("B").await.unwrap().len(), 1);
    }

    // --- list_sessions -----------------------------------------------------
    if has_list_sessions {
        // 7. returns session ids for the project; mtime is epoch-ms.
        let store = make_store();
        store.append(&key("proj", "a", None), &[entry(json_n("_", 1))]).await.unwrap();
        store.append(&key("proj", "b", None), &[entry(json_n("_", 1))]).await.unwrap();
        store.append(&key("other", "c", None), &[entry(json_n("_", 1))]).await.unwrap();
        let sessions = store.list_sessions("proj").await.unwrap();
        assert_eq!(ids(&sessions), vec!["a", "b"]);
        assert!(sessions.iter().all(|s| s.mtime as f64 > 1e12));
        assert!(store.list_sessions("never-appended").await.unwrap().is_empty());

        // 8. excludes subagent subpaths.
        let store = make_store();
        store.append(&key("proj", "main", None), &[entry(json_n("_", 1))]).await.unwrap();
        store.append(&key("proj", "main", Some("subagents/agent-1")), &[entry(json_n("_", 1))]).await.unwrap();
        assert_eq!(ids(&store.list_sessions("proj").await.unwrap()), vec!["main"]);
    }

    // --- list_session_summaries --------------------------------------------
    if has_list_summaries {
        let store = make_store();
        let sk = key("proj", "summ-sess", None);
        store
            .append(
                &sk,
                &[
                    entry(json!({"timestamp": "2024-01-01T00:00:00.000Z", "customTitle": "first"})),
                    entry(json!({"timestamp": "2024-01-01T00:00:01.000Z"})),
                ],
            )
            .await
            .unwrap();
        store
            .append(&sk, &[entry(json!({"timestamp": "2024-01-01T00:00:02.000Z", "customTitle": "second"}))])
            .await
            .unwrap();
        store
            .append(&key("other", "elsewhere", None), &[entry(json!({"timestamp": "2024-01-01T00:00:00.000Z"}))])
            .await
            .unwrap();

        let summaries = store.list_session_summaries("proj").await.unwrap();
        assert_eq!(summaries.len(), 1);
        let summ = &summaries[0];
        assert_eq!(summ.session_id, "summ-sess");
        assert!(summ.mtime as f64 > 1e12);
        if has_list_sessions {
            let ls = store.list_sessions("proj").await.unwrap();
            let ls_mtime = ls.iter().find(|e| e.session_id == "summ-sess").unwrap().mtime;
            assert!(summ.mtime >= ls_mtime);
        }
        // data round-trips through the fold; mtime is preserved by the fold.
        let refolded = fold_session_summary(
            Some(summ),
            &sk,
            &[entry(json!({"timestamp": "2024-01-01T00:00:03.000Z"}))],
        );
        assert_eq!(refolded.session_id, "summ-sess");
        assert_eq!(refolded.mtime, summ.mtime);
        let summ_data = summ.data.clone();

        // A subagent append must not affect the main session's summary.
        store
            .append(&key("proj", "summ-sess", Some("subagents/agent-1")), &[entry(json!({"timestamp": "2024-01-01T00:00:09.000Z", "customTitle": "subagent"}))])
            .await
            .unwrap();
        let after = store.list_session_summaries("proj").await.unwrap();
        assert_eq!(after.iter().find(|s| s.session_id == "summ-sess").unwrap().data, summ_data);
        assert!(store.list_session_summaries("never-appended").await.unwrap().is_empty());
        if has_delete {
            store.delete(&sk).await.unwrap();
            assert!(store.list_session_summaries("proj").await.unwrap().is_empty());
        }
    }

    // --- delete ------------------------------------------------------------
    if has_delete {
        // 9. delete main then load returns None (delete of absent is a no-op).
        let store = make_store();
        store.delete(&key("proj", "never-written", None)).await.unwrap();
        store.append(&k, &[entry(json_n("_", 1))]).await.unwrap();
        store.delete(&k).await.unwrap();
        assert_eq!(store.load(&k).await.unwrap(), None);

        // 10. delete main cascades to subkeys, spares other sessions/projects.
        let store = make_store();
        let sub1 = key("proj", "sess", Some("subagents/agent-1"));
        let sub2 = key("proj", "sess", Some("subagents/agent-2"));
        let other = key("proj", "sess2", None);
        let other_proj = key("other-proj", "sess", None);
        for kk in [&k, &sub1, &sub2, &other, &other_proj] {
            store.append(kk, &[entry(json_n("_", 1))]).await.unwrap();
        }
        store.delete(&k).await.unwrap();
        assert_eq!(store.load(&k).await.unwrap(), None);
        assert_eq!(store.load(&sub1).await.unwrap(), None);
        assert_eq!(store.load(&sub2).await.unwrap(), None);
        assert_eq!(store.load(&other).await.unwrap().map(|v| v.len()), Some(1));
        assert_eq!(store.load(&other_proj).await.unwrap().map(|v| v.len()), Some(1));
        if has_list_subkeys {
            assert!(store
                .list_subkeys(&SessionListSubkeysKey { project_key: "proj".into(), session_id: "sess".into() })
                .await
                .unwrap()
                .is_empty());
        }

        // 11. delete with a subpath removes only that subkey.
        let store = make_store();
        store.append(&k, &[entry(json_n("_", 1))]).await.unwrap();
        store.append(&sub1, &[entry(json_n("_", 1))]).await.unwrap();
        store.append(&sub2, &[entry(json_n("_", 1))]).await.unwrap();
        store.delete(&sub1).await.unwrap();
        assert_eq!(store.load(&sub1).await.unwrap(), None);
        assert_eq!(store.load(&sub2).await.unwrap().map(|v| v.len()), Some(1));
        assert_eq!(store.load(&k).await.unwrap().map(|v| v.len()), Some(1));
        if has_list_subkeys {
            assert_eq!(
                store
                    .list_subkeys(&SessionListSubkeysKey { project_key: "proj".into(), session_id: "sess".into() })
                    .await
                    .unwrap(),
                vec!["subagents/agent-2"]
            );
        }
    }

    // --- list_subkeys ------------------------------------------------------
    if has_list_subkeys {
        // 12. returns subpaths for the session, excludes other sessions.
        let store = make_store();
        store.append(&k, &[entry(json_n("_", 1))]).await.unwrap();
        store.append(&key("proj", "sess", Some("subagents/agent-1")), &[entry(json_n("_", 1))]).await.unwrap();
        store.append(&key("proj", "sess", Some("subagents/agent-2")), &[entry(json_n("_", 1))]).await.unwrap();
        store.append(&key("proj", "other-sess", Some("subagents/agent-x")), &[entry(json_n("_", 1))]).await.unwrap();
        let mut subkeys = store
            .list_subkeys(&SessionListSubkeysKey { project_key: "proj".into(), session_id: "sess".into() })
            .await
            .unwrap();
        subkeys.sort();
        assert_eq!(subkeys, vec!["subagents/agent-1", "subagents/agent-2"]);

        // 13. excludes the main transcript.
        let store = make_store();
        store.append(&k, &[entry(json_n("_", 1))]).await.unwrap();
        assert!(store
            .list_subkeys(&SessionListSubkeysKey { project_key: "proj".into(), session_id: "sess".into() })
            .await
            .unwrap()
            .is_empty());
    }
}

fn json_n(uuid: &str, n: i64) -> Value {
    json!({"uuid": uuid, "n": n})
}
