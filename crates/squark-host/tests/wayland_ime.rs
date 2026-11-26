// SPDX-License-Identifier: MIT

// Wayland and its Unix sockets exist only on Unix.
#![cfg(unix)]

#[path = "support/timeout.rs"]
mod timeout;

use std::{
    fs::{self, OpenOptions},
    io::{BufRead, BufReader, Write},
    os::{fd::AsFd, unix::net::UnixStream},
    path::Path,
    process::{Child, Command},
    thread,
    time::Duration,
};

use serde_json::{Value, json};
use wayland_client::{
    Connection, Dispatch, QueueHandle, delegate_noop,
    globals::{GlobalListContents, registry_queue_init},
    protocol::{wl_buffer, wl_compositor, wl_registry, wl_seat, wl_shm, wl_shm_pool, wl_surface},
};
use wayland_protocols_misc::zwp_input_method_v2::client::{
    zwp_input_method_manager_v2, zwp_input_method_v2, zwp_input_popup_surface_v2,
};

#[derive(Default)]
struct Method {
    active: bool,
    unavailable: bool,
    done: u32,
    area: Option<[i32; 4]>,
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for Method {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

delegate_noop!(Method: ignore wl_seat::WlSeat);
delegate_noop!(Method: ignore wl_compositor::WlCompositor);
delegate_noop!(Method: ignore wl_surface::WlSurface);
delegate_noop!(Method: ignore wl_shm::WlShm);
delegate_noop!(Method: ignore wl_shm_pool::WlShmPool);
delegate_noop!(Method: ignore wl_buffer::WlBuffer);
delegate_noop!(Method: ignore zwp_input_method_manager_v2::ZwpInputMethodManagerV2);

impl Dispatch<zwp_input_method_v2::ZwpInputMethodV2, ()> for Method {
    fn event(
        state: &mut Self,
        _: &zwp_input_method_v2::ZwpInputMethodV2,
        event: zwp_input_method_v2::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use zwp_input_method_v2::Event;
        match event {
            Event::Activate => state.active = true,
            Event::Deactivate => state.active = false,
            Event::Done => state.done += 1,
            Event::Unavailable => state.unavailable = true,
            _ => {}
        }
    }
}

impl Dispatch<zwp_input_popup_surface_v2::ZwpInputPopupSurfaceV2, ()> for Method {
    fn event(
        state: &mut Self,
        _: &zwp_input_popup_surface_v2::ZwpInputPopupSurfaceV2,
        event: zwp_input_popup_surface_v2::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwp_input_popup_surface_v2::Event::TextInputRectangle {
            x,
            y,
            width,
            height,
        } = event
        {
            state.area = Some([x, y, width, height]);
        }
    }
}

struct App {
    process: Child,
    deadline: Option<timeout::Deadline>,
    stream: BufReader<UnixStream>,
    request: u32,
}

impl App {
    fn start(binary: &Path, work: &Path) -> Self {
        let socket = work.join("squark.sock");
        let process = Command::new(binary)
            .args(["--width", "800", "--height", "600", "--test-control"])
            .arg(&socket)
            .env("XDG_DATA_HOME", work)
            .env_remove("DISPLAY")
            .spawn()
            .unwrap();
        let deadline = Some(timeout::Deadline::start(&process, Duration::from_secs(40)));
        let connection = (0..200).find_map(|_| match UnixStream::connect(&socket) {
            Ok(stream) => Some(stream),
            Err(_) => {
                thread::sleep(Duration::from_millis(15));
                None
            }
        });
        let stream = connection.expect("the Wayland app should open its control socket");
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        Self {
            process,
            deadline,
            stream: BufReader::new(stream),
            request: 0,
        }
    }

    fn rpc(&mut self, method: &str, params: Value) -> Value {
        self.request += 1;
        writeln!(
            self.stream.get_mut(),
            "{}",
            json!({"jsonrpc":"2.0", "id":self.request, "method":method, "params":params})
        )
        .unwrap();
        let mut line = String::new();
        self.stream
            .read_line(&mut line)
            .unwrap_or_else(|error| panic!("{method}: {error}"));
        let mut response: Value = serde_json::from_str(&line).unwrap();
        assert!(response.get("error").is_none(), "{response}");
        response["result"].take()
    }

    fn eval(&mut self, script: &str) -> Value {
        self.rpc("dom.eval", json!({"script":script}))["value"].take()
    }

    fn wait_for(&mut self, script: &str) {
        for _ in 0..120 {
            if self.eval(script) == true {
                return;
            }
            thread::sleep(Duration::from_millis(15));
        }
        panic!("timed out waiting for {script}");
    }
}

impl Drop for App {
    fn drop(&mut self) {
        let _ = writeln!(
            self.stream.get_mut(),
            r#"{{"jsonrpc":"2.0","id":0,"method":"app.quit","params":{{}}}}"#
        );
        let _ = self.process.wait();
        // Cancel the deadline only once the app has been reaped.
        self.deadline.take();
    }
}

/// Run in a Wayland session with no other input method. SQUARK_IME_REACT_APP is a
/// `squark build` binary for the unmodified apps/todomvc app.
#[test]
fn wayland_ime_commits_to_a_react_controlled_input() {
    if std::env::var("SQUARK_WINDOW_TESTS").as_deref() != Ok("1") {
        return;
    }
    let Some(binary) = std::env::var_os("SQUARK_IME_REACT_APP") else {
        return;
    };
    let work = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(format!("../../target/wayland-ime-{}", std::process::id()));
    fs::create_dir_all(&work).unwrap();
    let connection = Connection::connect_to_env().unwrap();
    let (globals, mut queue) = registry_queue_init::<Method>(&connection).unwrap();
    let handle = queue.handle();
    let manager: zwp_input_method_manager_v2::ZwpInputMethodManagerV2 =
        globals.bind(&handle, 1..=1, ()).unwrap();
    let seat: wl_seat::WlSeat = globals.bind(&handle, 1..=1, ()).unwrap();
    let compositor: wl_compositor::WlCompositor = globals.bind(&handle, 1..=1, ()).unwrap();
    let shm: wl_shm::WlShm = globals.bind(&handle, 1..=1, ()).unwrap();
    let input = manager.get_input_method(&seat, &handle, ());
    let surface = compositor.create_surface(&handle, ());
    let _popup = input.get_input_popup_surface(&surface, &handle, ());

    // Some compositors send the candidate rectangle only for a mapped popup.
    // A transparent pixel satisfies that lifecycle without drawing a candidate.
    let pixel = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(work.join("popup.rgba"))
        .unwrap();
    pixel.set_len(4).unwrap();
    let pool = shm.create_pool(pixel.as_fd(), 4, &handle, ());
    let buffer = pool.create_buffer(0, 1, 1, 4, wl_shm::Format::Argb8888, &handle, ());
    surface.attach(Some(&buffer), 0, 0);
    surface.damage(0, 0, 1, 1);
    surface.commit();

    let mut method = Method::default();
    queue.roundtrip(&mut method).unwrap();
    assert!(
        !method.unavailable,
        "use a session without a running input method"
    );

    let mut app = App::start(Path::new(&binary), &work);
    app.wait_for("!!document.querySelector('[data-testid=new-todo]')");
    app.eval(
        r#"(() => {
            window.field = document.querySelector('[data-testid=new-todo]');
            window.compositions = [];
            for (const type of ['compositionstart', 'compositionupdate', 'compositionend']) {
              field.addEventListener(type, e => compositions.push([e.type, e.data]));
            }
            field.focus();
        })()"#,
    );
    for _ in 0..100 {
        queue.roundtrip(&mut method).unwrap();
        if method.active && method.done > 0 && method.area.is_some_and(|area| area[2] <= 3) {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert!(
        method.active && method.done > 0,
        "text-input-v3 did not activate"
    );
    let [_, _, width, height] = method
        .area
        .expect("the candidate area should reach the IME");
    // The compositor reports the area in popup coordinates.
    assert!((1..=3).contains(&width) && height > 0);
    assert_eq!(
        app.eval(&format!("{height} <= field.getBoundingClientRect().height")),
        true,
    );

    input.set_preedit_string("😀に".into(), 4, 4);
    input.commit(method.done);
    queue.flush().unwrap();
    app.wait_for("field.value === '😀に' && field.selectionStart === 2");
    queue.roundtrip(&mut method).unwrap();
    input.commit_string("日本".into());
    input.commit(method.done);
    queue.flush().unwrap();
    app.wait_for("field.value === '日本' && compositions.at(-1)?.[0] === 'compositionend'");
    assert_eq!(
        app.eval("compositions"),
        json!([
            ["compositionstart", ""],
            ["compositionupdate", "😀に"],
            ["compositionupdate", "日本"],
            ["compositionend", "日本"]
        ])
    );
    app.rpc(
        "input.key",
        json!({"kind":"down", "key":"Enter", "code":"Enter", "modifiers":0}),
    );
    app.wait_for("document.querySelector('[data-testid=todo-label]')?.textContent === '日本'");

    app.eval("field.blur()");
    for _ in 0..100 {
        queue.roundtrip(&mut method).unwrap();
        if !method.active {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert!(!method.active, "blurring the field should disable the IME");
}
