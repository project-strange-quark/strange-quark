// SPDX-License-Identifier: MIT

use std::{
    sync::{Arc, mpsc},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use squark_gpu::{Error, WindowRenderer};
use winit::{
    application::ApplicationHandler,
    dpi::{LogicalSize, PhysicalSize},
    event::WindowEvent,
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy},
    window::{Window, WindowId},
};

use crate::scene;

const STARTUP_TIMEOUT: Duration = Duration::from_secs(15);
const VISIBLE_FOR: Duration = Duration::from_secs(3);

enum RenderEvent {
    FirstPresent,
    Retry,
    Failed(Error),
}

pub(crate) fn run() -> Result<(), Error> {
    let event_loop = EventLoop::<RenderEvent>::with_user_event().build()?;
    let mut app = App {
        proxy: event_loop.create_proxy(),
        window: None,
        render: None,
        deadline: Instant::now() + STARTUP_TIMEOUT,
        presented: false,
        error: None,
    };
    let result = event_loop.run_app(&mut app);
    if let Some(RenderThread { redraw, join }) = app.render.take() {
        drop(redraw);
        join.join().map_err(|_| "render thread panicked")?;
    }
    result?;
    app.error.map_or(Ok(()), Err)
}

struct RenderThread {
    redraw: mpsc::Sender<PhysicalSize<u32>>,
    join: JoinHandle<()>,
}

struct App {
    proxy: EventLoopProxy<RenderEvent>,
    window: Option<Arc<Window>>,
    render: Option<RenderThread>,
    deadline: Instant,
    presented: bool,
    error: Option<Error>,
}

impl App {
    fn show(&mut self, event_loop: &ActiveEventLoop) -> Result<(), Error> {
        let [width, height] = scene::SIZE;
        let window = Arc::new(
            event_loop.create_window(
                Window::default_attributes()
                    .with_title("squark-hello")
                    .with_inner_size(LogicalSize::new(width, height))
                    .with_resizable(false)
                    .with_visible(false),
            )?,
        );
        let (redraw, requests) = mpsc::channel();
        redraw.send(window.inner_size())?;
        let worker_window = Arc::clone(&window);
        let proxy = self.proxy.clone();
        let join = thread::Builder::new()
            .name("strange-quark".into())
            .spawn(move || {
                if let Err(error) = render(worker_window, requests, &proxy) {
                    let _ = proxy.send_event(RenderEvent::Failed(error));
                }
            })?;
        self.window = Some(window);
        self.render = Some(RenderThread { redraw, join });
        Ok(())
    }

    fn redraw(&self, size: PhysicalSize<u32>) {
        if let Some(render) = &self.render {
            let _ = render.redraw.send(size);
        }
    }
}

impl ApplicationHandler<RenderEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_none()
            && let Err(error) = self.show(event_loop)
        {
            self.error = Some(error);
            event_loop.exit();
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        let Some(window) = self.window.as_ref().filter(|window| window.id() == id) else {
            return;
        };
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => self.redraw(size),
            WindowEvent::RedrawRequested => self.redraw(window.inner_size()),
            _ => {}
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: RenderEvent) {
        match event {
            RenderEvent::FirstPresent => {
                if let Some(window) = &self.window {
                    window.set_visible(true);
                }
                self.presented = true;
                self.deadline = Instant::now() + VISIBLE_FOR;
                eprintln!("squark-hello presented its first frame");
            }
            RenderEvent::Retry => {
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }
            RenderEvent::Failed(error) => {
                self.error = Some(error);
                event_loop.exit();
            }
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if Instant::now() < self.deadline {
            event_loop.set_control_flow(ControlFlow::WaitUntil(self.deadline));
            return;
        }
        if self.presented {
            eprintln!("squark-hello closed on its timer");
        } else {
            self.error = Some("no frame was presented before the startup timeout".into());
        }
        event_loop.exit();
    }
}

fn render(
    window: Arc<Window>,
    requests: mpsc::Receiver<PhysicalSize<u32>>,
    proxy: &EventLoopProxy<RenderEvent>,
) -> Result<(), Error> {
    let mut renderer = pollster::block_on(WindowRenderer::new(
        Arc::clone(&window),
        &scene::frame(),
        scene::SIZE,
    ))?;
    eprintln!(
        "{} adapter: {}",
        squark_gpu::native_backend_name(),
        renderer.adapter_name()
    );

    let mut presented = false;
    while let Ok(mut size) = requests.recv() {
        while let Ok(latest) = requests.try_recv() {
            size = latest;
        }
        if size.width == 0 || size.height == 0 {
            continue;
        }

        let event =
            if renderer.present([size.width, size.height], || window.pre_present_notify())? {
                if presented {
                    continue;
                }
                presented = true;
                RenderEvent::FirstPresent
            } else {
                RenderEvent::Retry
            };
        if proxy.send_event(event).is_err() {
            break;
        }
    }
    Ok(())
}
