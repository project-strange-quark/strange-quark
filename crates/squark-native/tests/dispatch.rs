// SPDX-License-Identifier: MIT

use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    time::Duration,
};

use serde::{Deserialize, Serialize};
use squark_native::{
    App, CloseRequest, DialogRequest, Module, State, WindowOp, dispatcher, export, handlers,
};
use squark_types::native::{NativeCall, NativeDispatcher, NativeEvents, NativeHost};

#[derive(Default)]
struct Counter(AtomicUsize);

#[derive(Serialize, Deserialize, Debug, PartialEq)]
struct Entry {
    name: String,
    is_dir: bool,
}

#[export]
fn greet(full_name: String, times: Option<u32>) -> String {
    format!("Hello, {full_name}!").repeat(times.unwrap_or(1) as usize)
}

#[export]
fn bump(counter: State<Counter>) -> usize {
    counter.0.fetch_add(1, Ordering::SeqCst) + 1
}

#[export]
async fn list(app: App, session_id: String) -> Result<Vec<Entry>, String> {
    tokio::time::sleep(Duration::from_millis(5)).await;
    app.emit("listed", serde_json::json!({ "session_id": session_id }))
        .map_err(|error| error.to_string())?;
    Ok(vec![Entry {
        name: "src".into(),
        is_dir: true,
    }])
}

#[export]
fn fail(path: String) -> Result<(), String> {
    Err(format!("No such file: {path}"))
}

#[export]
fn boom() {
    panic!("boom");
}

#[export(rename = "pick_dir")]
fn pick(app: App) -> Option<PathBuf> {
    app.dialog(DialogRequest::PickFolder {
        title: None,
        directory: None,
    })
    .and_then(|paths| paths.into_iter().next())
}

#[export]
fn quit(app: App) {
    app.window().close();
}

mod nested {
    use squark_native::export;

    #[export(rename_all = "snake_case")]
    pub fn echo(raw_value: serde_json::Value) -> serde_json::Value {
        raw_value
    }
}

#[derive(Default)]
struct Events(Mutex<Vec<(String, String)>>);

impl NativeEvents for Events {
    fn emit(&self, event: &str, payload: String) {
        self.0.lock().unwrap().push((event.into(), payload));
    }
}

#[derive(Default)]
struct Host(Mutex<Vec<WindowOp>>);

impl NativeHost for Host {
    fn window(&self, op: WindowOp) {
        self.0.lock().unwrap().push(op);
    }
    fn dialog(&self, _request: DialogRequest) -> Option<Vec<PathBuf>> {
        Some(vec![PathBuf::from("/work/demo")])
    }
    fn open_external(&self, _url: &str) -> Result<(), String> {
        Ok(())
    }
}

fn call(
    dispatcher: &dyn NativeDispatcher,
    module: &str,
    command: &str,
    args: &str,
) -> Result<String, String> {
    let (send, receive) = mpsc::channel();
    dispatcher.invoke(
        NativeCall {
            module: module.into(),
            command: command.into(),
            args: args.into(),
        },
        Box::new(move |result| send.send(result).unwrap()),
    );
    receive
        .recv_timeout(Duration::from_secs(10))
        .expect("a reply")
}

fn modules() -> Vec<Module> {
    vec![
        Module::new("demo")
            .manage(Counter::default())
            .commands(handlers![
                greet,
                bump,
                list,
                fail,
                boom,
                pick,
                quit,
                nested::echo
            ])
            .on_close_requested(|app| {
                let _ = app.emit("close-requested", ());
                CloseRequest::Prevent
            }),
        Module::new("disabled").commands(handlers![greet]),
    ]
}

#[test]
fn commands_run_with_camel_case_args_state_events_and_errors() {
    let setups = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&setups);
    let mut list = modules();
    list[0] = std::mem::replace(&mut list[0], Module::new("x")).setup(move |_| {
        counted.fetch_add(1, Ordering::SeqCst);
    });
    let dispatcher = dispatcher(list);
    let host = Arc::new(Host::default());
    dispatcher.start(host.clone(), &["demo".into(), "missing".into()]);
    assert_eq!(setups.load(Ordering::SeqCst), 1);
    assert_eq!(dispatcher.modules(), ["demo"]);
    let events = Arc::new(Events::default());
    dispatcher.attach(events.clone());

    assert_eq!(
        call(
            &*dispatcher,
            "demo",
            "greet",
            r#"{"fullName":"Ada","times":2}"#
        ),
        Ok(r#""Hello, Ada!Hello, Ada!""#.into())
    );
    assert_eq!(
        call(&*dispatcher, "demo", "greet", r#"{"fullName":"Bo"}"#),
        Ok(r#""Hello, Bo!""#.into())
    );
    assert_eq!(call(&*dispatcher, "demo", "bump", "{}"), Ok("1".into()));
    assert_eq!(call(&*dispatcher, "demo", "bump", ""), Ok("2".into()));
    assert_eq!(
        call(&*dispatcher, "demo", "list", r#"{"sessionId":"s1"}"#),
        Ok(r#"[{"name":"src","is_dir":true}]"#.into())
    );
    assert_eq!(
        events.0.lock().unwrap().as_slice(),
        [("listed".to_string(), r#"{"session_id":"s1"}"#.to_string())]
    );
    assert_eq!(
        call(&*dispatcher, "demo", "fail", r#"{"path":"/x"}"#),
        Err(r#""No such file: /x""#.into())
    );
    assert_eq!(
        call(&*dispatcher, "demo", "boom", "{}"),
        Err(r#""command \"boom\" panicked""#.into())
    );
    assert_eq!(
        call(&*dispatcher, "demo", "pick_dir", "{}"),
        Ok(r#""/work/demo""#.into())
    );
    assert_eq!(
        call(&*dispatcher, "demo", "echo", r#"{"raw_value":[1,"a"]}"#),
        Ok(r#"[1,"a"]"#.into())
    );
    assert!(
        call(&*dispatcher, "demo", "greet", r#"{"times":1}"#)
            .unwrap_err()
            .contains("missing field `fullName`")
    );
    assert!(
        call(&*dispatcher, "demo", "nope", "{}")
            .unwrap_err()
            .contains("has no command")
    );
    assert!(
        call(&*dispatcher, "disabled", "greet", r#"{"fullName":"x"}"#)
            .unwrap_err()
            .contains("not enabled")
    );

    assert_eq!(dispatcher.close_requested(), CloseRequest::Prevent);
    assert_eq!(
        events.0.lock().unwrap().last().unwrap().0,
        "close-requested"
    );
    assert_eq!(call(&*dispatcher, "demo", "quit", "{}"), Ok("null".into()));
    assert_eq!(host.0.lock().unwrap().as_slice(), [WindowOp::Close]);
}

#[test]
fn events_before_a_document_attaches_are_dropped_and_state_can_be_added_later() {
    let dispatcher = dispatcher([Module::new("demo")]);
    dispatcher.app().emit("early", 1).unwrap();
    assert!(dispatcher.app().manage(5_u8));
    assert!(!dispatcher.app().manage(6_u8));
    assert_eq!(*dispatcher.app().state::<u8>().unwrap(), 5);
    assert!(dispatcher.app().state::<u16>().is_none());
    assert_eq!(dispatcher.close_requested(), CloseRequest::Allow);
}

#[test]
fn exit_handlers_run_once() {
    let runs = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&runs);
    let dispatcher = dispatcher([Module::new("demo").on_exit(move |_| {
        counted.fetch_add(1, Ordering::SeqCst);
    })]);
    dispatcher.exit();
    dispatcher.exit();
    assert_eq!(runs.load(Ordering::SeqCst), 1);
}

#[test]
fn menu_commands_reach_every_module_handler_off_the_caller_thread() {
    let (send, receive) = mpsc::channel();
    let caller = std::thread::current().id();
    let first = send.clone();
    let dispatcher = dispatcher([
        Module::new("a").on_menu(move |app, id| {
            assert_ne!(std::thread::current().id(), caller);
            app.window().set_menu(None);
            first.send(format!("a:{id}")).unwrap();
        }),
        Module::new("b").on_menu(move |_, id| send.send(format!("b:{id}")).unwrap()),
        Module::new("c").on_menu(|_, _| panic!("a broken handler")),
    ]);
    let host = Arc::new(Host::default());
    dispatcher.start(host.clone(), &["a".into()]);
    dispatcher.menu("file.save");
    let mut seen = [
        receive.recv_timeout(Duration::from_secs(10)).unwrap(),
        receive.recv_timeout(Duration::from_secs(10)).unwrap(),
    ];
    seen.sort();
    assert_eq!(seen, ["a:file.save", "b:file.save"]);
    assert_eq!(host.0.lock().unwrap().as_slice(), [WindowOp::SetMenu(None)]);
}

#[test]
fn menu_items_build_with_accelerators() {
    use squark_native::{Menu, MenuItem, MenuRole};
    let menu = Menu {
        items: vec![MenuItem::submenu(
            "&File",
            vec![
                MenuItem::command("file.open", "Open…").with_accelerator("CmdOrCtrl+O"),
                MenuItem::Separator,
                MenuItem::role(MenuRole::Quit),
                MenuItem::Separator.with_accelerator("Ctrl+X"),
            ],
        )],
    };
    let MenuItem::Submenu { items, .. } = &menu.items[0] else {
        panic!("a submenu")
    };
    assert_eq!(
        items[0],
        MenuItem::Command {
            id: "file.open".into(),
            label: "Open…".into(),
            accelerator: Some("CmdOrCtrl+O".into()),
            enabled: true,
            checked: None,
        }
    );
    assert_eq!(items[3], MenuItem::Separator);
}
