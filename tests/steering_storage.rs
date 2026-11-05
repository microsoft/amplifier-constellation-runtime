use agent_runtime::{config::Mode, steering::Inbox, store::Store};
use serde_json::json;
use std::{
    sync::{Arc, Barrier, mpsc},
    time::Duration,
};

#[test]
fn uncertainty_is_bounded_metadata_without_replaying_or_acknowledging_source_text() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().to_owned()).unwrap();
    let session = store.create(dir.path().to_owned(), Mode::ReadOnly).unwrap();
    let inbox = Inbox::new(store.clone(), &session.id).unwrap();
    for index in 0..12 {
        inbox
            .admit(
                &format!("input-{index}"),
                json!({"role":"user","content":"PRIVATE-uncertain-request"}),
            )
            .unwrap();
    }
    inbox
        .state(&inbox.queued().unwrap(), "submitted", None)
        .unwrap();
    inbox.close("disconnected").unwrap();
    let summary = store.input_uncertainty(&session.id).unwrap();
    assert_eq!(summary["count"], 12);
    assert_eq!(summary["items"].as_array().unwrap().len(), 8);
    assert_eq!(summary["complete"], false);
    assert!(!summary.to_string().contains("PRIVATE-uncertain-request"));
    assert!(store.load(&session.id).unwrap().items.is_empty());
    assert!(
        store
            .input_receipts(&session.id)
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["state"] == "unknown" && r["reconciled"] == false)
    );
}

#[test]
fn concurrent_history_and_receipts_finish_and_survive_reopening() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().to_owned()).unwrap();
    let session = store.create(dir.path().to_owned(), Mode::ReadOnly).unwrap();
    let inbox = Arc::new(Inbox::new(store.clone(), &session.id).unwrap());
    let barrier = Arc::new(Barrier::new(8));
    let (tx, rx) = mpsc::channel();
    let mut workers = Vec::new();
    for worker in 0..8 {
        let store = store.clone();
        let inbox = inbox.clone();
        let barrier = barrier.clone();
        let tx = tx.clone();
        let sid = session.id.clone();
        workers.push(std::thread::spawn(move || {
            barrier.wait();
            for index in 0..200 {
                if worker % 2 == 0 && index < 32 {
                    let id = format!("{worker}-{index}");
                    inbox
                        .admit(&id, json!({"role":"user","content":id}))
                        .unwrap();
                } else if worker % 2 == 0 {
                    store.input_receipts(&sid).unwrap();
                } else {
                    store
                        .event(
                            &sid,
                            None,
                            "fixture",
                            json!({"worker":worker,"index":index}),
                        )
                        .unwrap();
                    store.load(&sid).unwrap();
                }
            }
            tx.send(()).unwrap();
        }));
    }
    for _ in 0..8 {
        rx.recv_timeout(Duration::from_secs(30))
            .expect("Concurrent storage operations stalled");
    }
    for worker in workers {
        worker.join().unwrap();
    }
    assert_eq!(
        store
            .input_receipts(&session.id)
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        128
    );
    inbox.close("fixture_complete").unwrap();
    let ids: Vec<_> = store
        .input_receipts(&session.id)
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["inputId"].as_str().unwrap().to_owned())
        .collect();
    store.acknowledge_inputs(&session.id, &ids).unwrap();
    drop(inbox);
    drop(store);
    let reopened = Store::open(dir.path().to_owned()).unwrap();
    let receipts = reopened.input_receipts(&session.id).unwrap();
    assert_eq!(receipts.as_array().unwrap().len(), 128);
    assert!(
        receipts
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["state"] == "notAccepted" && r["reconciled"] == true)
    );
    assert!(
        reopened
            .events(&session.id)
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["kind"] == "input.reconciled")
    );
}
