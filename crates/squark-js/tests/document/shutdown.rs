// SPDX-License-Identifier: MIT

use super::*;
use squark_js::DocumentShutdown;
use squark_types::render::ResourceSendError;

const WAIT: Duration = Duration::from_secs(2);

#[test]
fn queued_shutdown_rejects_reads_without_running_the_batch() {
    support::initialize_v8();
    let origin = Instant::now();
    let trace = Trace::default();
    let runner = DocumentRunner::new(
        options(origin, true),
        &fixture(&[]),
        sources(|_| None),
        Box::new(Engine::new(trace.clone())),
        Box::new(Paint(trace.clone())),
        Instant::now,
    )
    .unwrap();
    trace.borrow_mut().clear();

    let (send, receiver) = mpsc::channel();
    let (reply, eval) = mpsc::channel();
    send.send(DocumentTask::Eval {
        script: "document.body.offsetWidth".into(),
        reply,
    })
    .unwrap();
    send.send(tick(origin, 1, 16).into()).unwrap();
    send.send(DocumentTask::Shutdown).unwrap();
    let (reply, rect) = mpsc::channel();
    send.send(DocumentTask::QueryRect {
        selector: "body".into(),
        reply,
    })
    .unwrap();
    runner
        .run(receiver, |_| panic!("painted after shutdown"))
        .unwrap();

    assert!(matches!(
        eval.recv_timeout(WAIT),
        Ok(Err(DocumentError::Closed))
    ));
    assert!(matches!(
        rect.recv_timeout(WAIT),
        Ok(Err(DocumentError::Closed))
    ));
    assert!(trace.borrow().is_empty());
}

#[test]
fn close_during_a_sync_read_aborts_javascript_and_rejects_pending_reads() {
    support::initialize_v8();
    let origin = Instant::now();
    let trace = Trace::default();
    let shutdown = DocumentShutdown::default();
    let (entered, reading) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let mut engine = Engine::new(trace.clone());
    engine.before_query = Some(Box::new(move || {
        entered.send(()).unwrap();
        released.recv_timeout(WAIT).unwrap();
        Ok(())
    }));
    let runner = DocumentRunner::new(
        options(origin, true),
        &fixture(&[]),
        sources(|_| None).with_shutdown(shutdown.clone()),
        Box::new(engine),
        Box::new(Paint(trace.clone())),
        Instant::now,
    )
    .unwrap();
    trace.borrow_mut().clear();
    let (send, receiver) = mpsc::channel();
    let (reply, eval) = mpsc::channel();
    send.send(DocumentTask::Eval {
        script: "document.body.offsetWidth; document.body.offsetHeight; while (true) {}".into(),
        reply,
    })
    .unwrap();
    let (reply, rect) = mpsc::channel();
    send.send(DocumentTask::QueryRect {
        selector: "body".into(),
        reply,
    })
    .unwrap();
    std::thread::scope(|scope| {
        scope.spawn(move || {
            reading.recv_timeout(WAIT).unwrap();
            shutdown.close();
            send.send(DocumentTask::Shutdown).unwrap();
            release.send(()).unwrap();
        });
        runner
            .run(receiver, |_| panic!("painted after shutdown"))
            .unwrap();
    });
    assert!(matches!(
        eval.recv_timeout(WAIT),
        Ok(Err(DocumentError::Closed))
    ));
    assert!(matches!(
        rect.recv_timeout(WAIT),
        Ok(Err(DocumentError::Closed))
    ));
    assert_eq!(*trace.borrow(), ["query:0"]);
}

#[test]
fn render_disconnect_closes_a_document_even_before_its_shutdown_message() {
    support::initialize_v8();
    let origin = Instant::now();
    let trace = Trace::default();
    let mut engine = Engine::new(trace.clone());
    engine.before_query = Some(Box::new(|| {
        Err(EngineError::Resource(
            ResourceSendError::Disconnected.to_string(),
        ))
    }));
    let mut runner = DocumentRunner::new(
        options(origin, true),
        &fixture(&[]),
        sources(|_| None),
        Box::new(engine),
        Box::new(Paint(trace.clone())),
        Instant::now,
    )
    .unwrap();
    trace.borrow_mut().clear();
    assert!(matches!(
        runner.eval("document.body.offsetWidth; document.body.offsetHeight"),
        Err(DocumentError::Closed)
    ));
    assert!(matches!(
        runner.query_rect("body"),
        Err(DocumentError::Closed)
    ));
    assert_eq!(*trace.borrow(), ["query:0"]);
}
