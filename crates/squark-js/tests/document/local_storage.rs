// SPDX-License-Identifier: MIT

use std::{
    process::Command,
    sync::atomic::{AtomicU32, Ordering},
    thread,
    time::Instant,
};

use super::*;

const APP: &str = "local-storage-test";

fn root() -> std::path::PathBuf {
    static SEQUENCE: AtomicU32 = AtomicU32::new(0);
    key(&format!(
        "target/dom-local-storage-{}-{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ))
}

fn make_runner(root: &Path) -> DocumentRunner<'static> {
    support::initialize_v8();
    let origin = Instant::now();
    let trace = Trace::default();
    let sources = sources(|path| {
        (path == squark_types::pack::MANIFEST_PATH).then(|| {
            (
                "application/json".to_owned(),
                format!(r#"{{"app_name":"{APP}"}}"#).into_bytes(),
            )
        })
    })
    .with_storage_root(root.to_owned());
    DocumentRunner::new(
        options(origin, true),
        &fixture(&[]),
        sources,
        Box::new(Engine::new(trace.clone())),
        Box::new(Paint(trace)),
        Instant::now,
    )
    .unwrap()
}

fn quit(runner: DocumentRunner<'_>) {
    let (send, receive) = mpsc::channel();
    send.send(DocumentTask::Shutdown).unwrap();
    runner
        .run(receive, |_| panic!("frame during quit"))
        .unwrap();
}

#[test]
fn local_storage_writes_are_visible_to_the_page_immediately() {
    let root = root();
    let mut runner = make_runner(&root);
    assert_eq!(
        runner
            .eval(
                r#"
        localStorage.setItem("scene", "saved");
        const visible = [localStorage.getItem("scene"), localStorage.length, localStorage.key(0)];
        localStorage.removeItem("scene");
        visible.push(localStorage.getItem("scene"), localStorage.length);
        localStorage.setItem("one", "1");
        localStorage.clear();
        visible.push(localStorage.length, localStorage.key(0));
        visible;
    "#,
            )
            .unwrap(),
        r#"["saved",1,"scene",null,0,0,null]"#
    );
    quit(runner);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn local_storage_set_item_does_not_block_on_persistence() {
    let root = root();
    let mut runner = make_runner(&root);
    let started = Instant::now();
    let result = runner
        .eval(
            r#"
        const value = "x".repeat(4096);
        for (let index = 0; index < 1000; index++) localStorage.setItem(`key${index}`, value);
        [localStorage.length, localStorage.getItem("key999").length];
    "#,
        )
        .unwrap();
    let elapsed = started.elapsed();
    assert_eq!(result, "[1000,4096]");
    assert!(
        elapsed < Duration::from_secs(1),
        "1,000 localStorage writes took {elapsed:?} on the document thread"
    );
    quit(runner);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn local_storage_is_shared_between_documents_in_one_process() {
    let root = root();
    let (ready, wait_for_second) = mpsc::channel();
    let (finish_first, finish) = mpsc::channel();
    let first_root = root.clone();
    let first = thread::spawn(move || {
        let mut runner = make_runner(&first_root);
        runner
            .eval(r#"localStorage.setItem("shared", "visible");"#)
            .unwrap();
        ready.send(()).unwrap();
        finish.recv().unwrap();
        drop(runner);
    });
    wait_for_second.recv().unwrap();
    let second_root = root.join("..").join(root.file_name().unwrap());
    let second = thread::spawn(move || {
        let mut runner = make_runner(&second_root);
        let result = runner.eval(r#"localStorage.getItem("shared")"#).unwrap();
        drop(runner);
        result
    });
    let result = second.join().unwrap();
    finish_first.send(()).unwrap();
    first.join().unwrap();
    assert_eq!(result, r#""visible""#);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn local_storage_drop_flushes_a_write_before_a_new_document_starts() {
    let root = root();
    let mut runner = make_runner(&root);
    runner
        .eval(r#"localStorage.setItem("drop", "persisted");"#)
        .unwrap();
    drop(runner);

    let mut reopened = make_runner(&root);
    assert_eq!(
        reopened.eval(r#"localStorage.getItem("drop")"#).unwrap(),
        r#""persisted""#
    );
    drop(reopened);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn local_storage_quit_flushes_a_write_before_a_new_document_starts() {
    let root = root();
    let mut runner = make_runner(&root);
    runner
        .eval(r#"localStorage.setItem("quit", "persisted");"#)
        .unwrap();
    quit(runner);

    let mut reopened = make_runner(&root);
    assert_eq!(
        reopened.eval(r#"localStorage.getItem("quit")"#).unwrap(),
        r#""persisted""#
    );
    drop(reopened);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn local_storage_merges_writes_from_two_handles() {
    let root = root();
    let first_root = root.clone();
    let first = thread::spawn(move || {
        let mut runner = make_runner(&first_root);
        runner
            .eval(r#"localStorage.setItem("first", "one");"#)
            .unwrap();
        quit(runner);
    });
    let second_root = root.clone();
    let second = thread::spawn(move || {
        let mut runner = make_runner(&second_root);
        runner
            .eval(r#"localStorage.setItem("second", "two");"#)
            .unwrap();
        quit(runner);
    });
    first.join().unwrap();
    second.join().unwrap();

    let mut reopened = make_runner(&root);
    assert_eq!(
        reopened
            .eval(r#"[localStorage.getItem("first"), localStorage.getItem("second")]"#,)
            .unwrap(),
        r#"["one","two"]"#
    );
    drop(reopened);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn child_process_reads_every_key_after_local_storage_quit() {
    const CHILD_ROOT: &str = "SQUARK_LOCAL_STORAGE_CHILD_ROOT";
    const CHILD_COUNT: &str = "SQUARK_LOCAL_STORAGE_CHILD_COUNT";
    if let Some(root) = std::env::var_os(CHILD_ROOT) {
        let count = std::env::var(CHILD_COUNT)
            .unwrap()
            .parse::<usize>()
            .unwrap();
        let mut runner = make_runner(std::path::Path::new(&root));
        let script = format!(
            r#"localStorage.length === {count} &&
              Array.from({{ length: {count} }}, (_, index) =>
                localStorage.getItem("key" + index) === "value-" + index).every(Boolean)"#
        );
        assert_eq!(runner.eval(&script).unwrap(), "true");
        return;
    }

    let root = root();
    let mut runner = make_runner(&root);
    runner
        .eval(
            r#"
        for (let index = 0; index < 1000; index++) {
          localStorage.setItem(`key${index}`, `value-${index}`);
        }
    "#,
        )
        .unwrap();
    quit(runner);

    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "local_storage_tests::child_process_reads_every_key_after_local_storage_quit",
            "--nocapture",
        ])
        .env(CHILD_ROOT, &root)
        .env(CHILD_COUNT, "1000")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "child storage check failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    std::fs::remove_dir_all(root).unwrap();
}
