// SPDX-License-Identifier: MIT

use std::{collections::HashMap, time::Instant};

use squark_paint::{HitSnapshot, hit_test};
use squark_types::{
    NodeId, Point,
    fragment::Cursor,
    host::{DeltaMode, Input, InputTask, KeyKind, Modifiers, PointerKind, PointerType},
    render::Viewport,
};
use winit::{
    dpi::PhysicalPosition,
    event::{
        DeviceId, ElementState, KeyEvent, MouseButton, MouseScrollDelta, Touch, TouchPhase,
        WindowEvent,
    },
    keyboard::{Key, KeyCode, KeyLocation, ModifiersState, NamedKey, PhysicalKey},
    window::{CursorIcon, Window},
};

pub(crate) struct InputRouter {
    modifiers: Modifiers,
    mouse: Option<PhysicalPosition<f64>>,
    inside: bool,
    buttons: u16,
    mouse_target: NodeId,
    touches: HashMap<(DeviceId, u64), Contact>,
    touch_id: i32,
    request: u32,
    cursor: Cursor,
}

impl Default for InputRouter {
    fn default() -> Self {
        Self {
            modifiers: Modifiers::default(),
            mouse: None,
            inside: false,
            buttons: 0,
            mouse_target: NodeId::NONE,
            touches: HashMap::new(),
            touch_id: 1,
            request: 0,
            cursor: Cursor::Auto,
        }
    }
}

#[derive(Clone, Copy)]
struct Contact {
    id: i32,
    primary: bool,
    target: NodeId,
}

struct InputViewport<'a> {
    viewport: Viewport,
    snapshot: Option<&'a HitSnapshot>,
}

impl InputViewport<'_> {
    fn css(&self, physical: PhysicalPosition<f64>) -> Point {
        let logical = physical.to_logical::<f32>(self.viewport.scale);
        Point {
            x: logical.x,
            y: logical.y,
        }
    }

    fn target(&self, css: Point) -> NodeId {
        self.snapshot
            .and_then(|snapshot| hit_test(snapshot, css))
            .map_or(NodeId::NONE, |hit| hit.target)
    }
}

impl InputRouter {
    pub fn mouse_position(&self) -> Option<PhysicalPosition<f64>> {
        self.mouse.filter(|_| self.inside)
    }

    pub fn event(
        &mut self,
        event: &WindowEvent,
        time: Instant,
        viewport: Option<Viewport>,
        snapshot: Option<&HitSnapshot>,
    ) -> Option<InputTask> {
        if let WindowEvent::ModifiersChanged(modifiers) = event {
            self.modifiers = input_modifiers(modifiers.state());
            return None;
        }
        let view = InputViewport {
            viewport: viewport?,
            snapshot: snapshot.filter(|hit| Some(hit.viewport) == viewport),
        };
        let (target, related, input) = match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.mouse = Some(*position);
                self.inside = true;
                let css = view.css(*position);
                let target = view.target(css);
                let old = std::mem::replace(&mut self.mouse_target, target);
                (
                    target,
                    changed_target(old, target),
                    self.mouse_event(PointerKind::Move, css, -1),
                )
            }
            WindowEvent::CursorLeft { .. } => {
                self.inside = false;
                let css = view.css(self.mouse?);
                let old = std::mem::replace(&mut self.mouse_target, NodeId::NONE);
                (
                    NodeId::NONE,
                    old,
                    self.mouse_event(PointerKind::Move, css, -1),
                )
            }
            WindowEvent::MouseInput { state, button, .. } => {
                let css = view.css(self.mouse?);
                let target = if self.inside {
                    view.target(css)
                } else {
                    NodeId::NONE
                };
                let button = mouse_button(*button)?;
                let mask = button_mask(button)?;
                let kind = if state.is_pressed() {
                    self.buttons |= mask;
                    PointerKind::Down
                } else {
                    self.buttons &= !mask;
                    PointerKind::Up
                };
                self.mouse_target = target;
                (target, NodeId::NONE, self.mouse_event(kind, css, button))
            }
            WindowEvent::MouseWheel { delta, .. } if self.inside => {
                let css = view.css(self.mouse?);
                let (delta, mode) = match delta {
                    MouseScrollDelta::LineDelta(x, y) => {
                        ([-f64::from(*x), -f64::from(*y), 0.0], DeltaMode::Line)
                    }
                    MouseScrollDelta::PixelDelta(delta) => (
                        [
                            -delta.x / view.viewport.scale,
                            -delta.y / view.viewport.scale,
                            0.0,
                        ],
                        DeltaMode::Pixel,
                    ),
                };
                (
                    view.target(css),
                    NodeId::NONE,
                    Input::Wheel { css, delta, mode },
                )
            }
            WindowEvent::KeyboardInput { event, .. } => {
                (NodeId::NONE, NodeId::NONE, key_event(event))
            }
            WindowEvent::Touch(touch) => self.touch_event(touch, &view)?,
            WindowEvent::Focused(false) => {
                self.modifiers = Modifiers::default();
                if std::mem::take(&mut self.buttons) == 0 {
                    return None;
                }
                let css = view.css(self.mouse.unwrap_or_default());
                (
                    self.mouse_target,
                    NodeId::NONE,
                    self.mouse_event(PointerKind::Cancel, css, -1),
                )
            }
            _ => return None,
        };
        Some(self.make_task(target, related, time, self.modifiers, input))
    }

    pub fn synthetic(
        &mut self,
        request: crate::control::Request,
        time: Instant,
        viewport: Viewport,
        snapshot: Option<&HitSnapshot>,
    ) -> InputTask {
        use crate::control::{KeyKind as WireKey, PointerKind as WirePointer, Request};

        let view = InputViewport {
            viewport,
            snapshot: snapshot.filter(|hit| hit.viewport == viewport),
        };
        let (target, related, modifiers, input) = match request {
            Request::Pointer(args) => {
                let css = Point {
                    x: args.x,
                    y: args.y,
                };
                let target = view.target(css);
                self.mouse = Some(PhysicalPosition::new(
                    f64::from(css.x) * viewport.scale,
                    f64::from(css.y) * viewport.scale,
                ));
                self.inside = true;
                self.buttons = args.buttons;
                let previous = std::mem::replace(&mut self.mouse_target, target);
                let (kind, related) = match args.kind {
                    WirePointer::Down => (PointerKind::Down, NodeId::NONE),
                    WirePointer::Move => (PointerKind::Move, changed_target(previous, target)),
                    WirePointer::Up => (PointerKind::Up, NodeId::NONE),
                };
                let mut input = self.mouse_event(
                    kind,
                    css,
                    if kind == PointerKind::Move {
                        -1
                    } else {
                        i16::from(args.button)
                    },
                );
                if let Input::Pointer {
                    id, click_count, ..
                } = &mut input
                {
                    *id = args.pointer_id;
                    *click_count = args.click_count;
                }
                (target, related, args.modifiers, input)
            }
            Request::Wheel(args) => {
                let css = Point {
                    x: args.x,
                    y: args.y,
                };
                (
                    view.target(css),
                    NodeId::NONE,
                    args.modifiers,
                    Input::Wheel {
                        css,
                        delta: [args.dx, args.dy, 0.0],
                        mode: DeltaMode::Pixel,
                    },
                )
            }
            Request::Key(args) => {
                let location = if args.code.starts_with("Numpad") {
                    3
                } else if args.code.ends_with("Left") && args.code != "ArrowLeft" {
                    1
                } else if args.code.ends_with("Right") && args.code != "ArrowRight" {
                    2
                } else {
                    0
                };
                (
                    NodeId::NONE,
                    NodeId::NONE,
                    args.modifiers,
                    Input::Key {
                        kind: match args.kind {
                            WireKey::Down => KeyKind::Down,
                            WireKey::Up => KeyKind::Up,
                        },
                        key: args.key,
                        code: args.code,
                        location,
                        repeat: false,
                        composing: false,
                    },
                )
            }
            _ => unreachable!("expected an input request"),
        };
        self.make_task(
            target,
            related,
            time,
            Modifiers {
                shift: modifiers & 1 != 0,
                ctrl: modifiers & 2 != 0,
                alt: modifiers & 4 != 0,
                meta: modifiers & 8 != 0,
            },
            input,
        )
    }

    fn make_task(
        &mut self,
        target: NodeId,
        related: NodeId,
        time: Instant,
        modifiers: Modifiers,
        input: Input,
    ) -> InputTask {
        self.request = self.request.wrapping_add(1).max(1);
        InputTask {
            request: self.request,
            target,
            related,
            time,
            modifiers,
            input,
        }
    }

    pub fn ime(&mut self, edit: crate::ime::Edit) -> InputTask {
        self.make_task(
            NodeId::NONE,
            NodeId::NONE,
            edit.time,
            self.modifiers,
            Input::Ime {
                kind: edit.kind,
                text: edit.text,
                selection_utf16: edit.selection_utf16,
            },
        )
    }

    fn mouse_event(&self, kind: PointerKind, css: Point, button: i16) -> Input {
        let pressure = if self.buttons == 0 { 0.0 } else { 0.5 };
        Input::Pointer {
            kind,
            boundary_update: false,
            click_count: None,
            id: 1,
            device: PointerType::Mouse,
            primary: true,
            css,
            pressure,
            button,
            buttons: self.buttons,
            samples: Box::new([]),
        }
    }

    fn touch_event(
        &mut self,
        touch: &Touch,
        view: &InputViewport<'_>,
    ) -> Option<(NodeId, NodeId, Input)> {
        let key = (touch.device_id, touch.id);
        let css = view.css(touch.location);
        let target = view.target(css);
        let (kind, contact, related) = match touch.phase {
            TouchPhase::Started => {
                self.touch_id = self.touch_id.wrapping_add(1).max(2);
                let contact = Contact {
                    id: self.touch_id,
                    primary: self.touches.is_empty(),
                    target,
                };
                self.touches.insert(key, contact);
                (PointerKind::Down, contact, NodeId::NONE)
            }
            TouchPhase::Moved => {
                let contact = self.touches.get_mut(&key)?;
                let old = std::mem::replace(&mut contact.target, target);
                (PointerKind::Move, *contact, changed_target(old, target))
            }
            TouchPhase::Ended => (PointerKind::Up, self.touches.remove(&key)?, NodeId::NONE),
            TouchPhase::Cancelled => (
                PointerKind::Cancel,
                self.touches.remove(&key)?,
                NodeId::NONE,
            ),
        };
        let active = matches!(kind, PointerKind::Down | PointerKind::Move);
        let pressure = if active {
            touch
                .force
                .as_ref()
                .map(|force| force.normalized())
                .filter(|pressure| pressure.is_finite())
                .unwrap_or(0.5)
                .clamp(0.0, 1.0) as f32
        } else {
            0.0
        };
        let button = if matches!(kind, PointerKind::Down | PointerKind::Up) {
            0
        } else {
            -1
        };
        Some((
            target,
            related,
            Input::Pointer {
                kind,
                boundary_update: false,
                click_count: None,
                id: contact.id,
                device: PointerType::Touch,
                primary: contact.primary,
                css,
                pressure,
                button,
                buttons: u16::from(active),
                samples: Box::new([]),
            },
        ))
    }

    pub fn refresh_cursor(
        &mut self,
        window: &Window,
        viewport: Option<Viewport>,
        snapshot: Option<&HitSnapshot>,
    ) {
        let cursor = self.cursor_at(viewport, snapshot);
        if std::mem::replace(&mut self.cursor, cursor) != cursor {
            window.set_cursor_visible(cursor != Cursor::None);
            if let Some(icon) = cursor_icon(cursor) {
                window.set_cursor(icon);
            }
        }
    }

    fn cursor_at(&self, viewport: Option<Viewport>, snapshot: Option<&HitSnapshot>) -> Cursor {
        self.mouse
            .zip(snapshot)
            .filter(|(_, hit)| self.inside && Some(hit.viewport) == viewport)
            .and_then(|(position, hit)| {
                let view = InputViewport {
                    viewport: hit.viewport,
                    snapshot: Some(hit),
                };
                hit_test(hit, view.css(position))
            })
            .map_or(Cursor::Auto, |hit| hit.cursor)
    }
}

/// Remembers the mouse inputs sent to the document in render-thread order.
#[derive(Default)]
pub(crate) struct PresentedMouse {
    pointer: Option<MouseState>,
    modifiers: Modifiers,
}

struct MouseState {
    position: PhysicalPosition<f64>,
    id: i32,
    target: NodeId,
    buttons: u16,
    pressure: f32,
}

impl PresentedMouse {
    pub fn record(&mut self, task: &mut InputTask, position: Option<PhysicalPosition<f64>>) {
        self.modifiers = task.modifiers;
        let Input::Pointer {
            device: PointerType::Mouse,
            id,
            kind,
            buttons,
            pressure,
            ..
        } = task.input
        else {
            return;
        };
        if kind == PointerKind::Move
            && let Some(previous) = self.pointer.as_ref().filter(|mouse| mouse.id == id)
        {
            task.related = changed_target(previous.target, task.target);
        }
        self.pointer = position.map(|position| MouseState {
            position,
            id,
            target: task.target,
            buttons,
            pressure,
        });
    }

    pub fn boundary_update(&mut self, snapshot: &HitSnapshot, time: Instant) -> Option<InputTask> {
        let mouse = self.pointer.as_mut()?;
        let view = InputViewport {
            viewport: snapshot.viewport,
            snapshot: Some(snapshot),
        };
        let css = view.css(mouse.position);
        let target = view.target(css);
        if target == mouse.target {
            return None;
        }
        let related = std::mem::replace(&mut mouse.target, target);
        Some(InputTask {
            // This internal input has no waiting reply.
            request: 0,
            target,
            related,
            time,
            modifiers: self.modifiers,
            input: Input::Pointer {
                kind: PointerKind::Move,
                boundary_update: true,
                click_count: None,
                id: mouse.id,
                device: PointerType::Mouse,
                primary: true,
                css,
                pressure: mouse.pressure,
                button: -1,
                buttons: mouse.buttons,
                samples: Box::new([]),
            },
        })
    }
}

fn changed_target(old: NodeId, new: NodeId) -> NodeId {
    if old == new { NodeId::NONE } else { old }
}

fn mouse_button(button: MouseButton) -> Option<i16> {
    Some(match button {
        MouseButton::Left => 0,
        MouseButton::Middle => 1,
        MouseButton::Right => 2,
        MouseButton::Back => 3,
        MouseButton::Forward => 4,
        MouseButton::Other(index) if index >= 5 => i16::try_from(index).ok()?,
        MouseButton::Other(_) => return None,
    })
}

fn button_mask(button: i16) -> Option<u16> {
    let bit = match button {
        1 => 2,
        2 => 1,
        other => other,
    };
    1u16.checked_shl(u32::try_from(bit).ok()?)
}

fn input_modifiers(state: ModifiersState) -> Modifiers {
    Modifiers {
        alt: state.alt_key(),
        ctrl: state.control_key(),
        meta: state.super_key(),
        shift: state.shift_key(),
    }
}

fn key_event(event: &KeyEvent) -> Input {
    let key = match &event.logical_key {
        Key::Character(key) => key.to_string(),
        Key::Named(NamedKey::Space) => " ".into(),
        Key::Named(NamedKey::Super) => "Meta".into(),
        Key::Named(key) => format!("{key:?}"),
        Key::Dead(_) => "Dead".into(),
        Key::Unidentified(_) => "Unidentified".into(),
    };
    let code = match event.physical_key {
        PhysicalKey::Code(KeyCode::SuperLeft) => "MetaLeft".into(),
        PhysicalKey::Code(KeyCode::SuperRight) => "MetaRight".into(),
        PhysicalKey::Code(code) => format!("{code:?}"),
        PhysicalKey::Unidentified(_) => "Unidentified".into(),
    };
    let location = match event.location {
        KeyLocation::Standard => 0,
        KeyLocation::Left => 1,
        KeyLocation::Right => 2,
        KeyLocation::Numpad => 3,
    };
    Input::Key {
        kind: match event.state {
            ElementState::Pressed => KeyKind::Down,
            ElementState::Released => KeyKind::Up,
        },
        key,
        code,
        location,
        repeat: event.repeat,
        composing: false,
    }
}

fn cursor_icon(cursor: Cursor) -> Option<CursorIcon> {
    Some(match cursor {
        Cursor::None => return None,
        Cursor::Auto | Cursor::Default => CursorIcon::Default,
        Cursor::ContextMenu => CursorIcon::ContextMenu,
        Cursor::Help => CursorIcon::Help,
        Cursor::Pointer => CursorIcon::Pointer,
        Cursor::Progress => CursorIcon::Progress,
        Cursor::Wait => CursorIcon::Wait,
        Cursor::Cell => CursorIcon::Cell,
        Cursor::Crosshair => CursorIcon::Crosshair,
        Cursor::Text => CursorIcon::Text,
        Cursor::VerticalText => CursorIcon::VerticalText,
        Cursor::Alias => CursorIcon::Alias,
        Cursor::Copy => CursorIcon::Copy,
        Cursor::Move => CursorIcon::Move,
        Cursor::NoDrop => CursorIcon::NoDrop,
        Cursor::NotAllowed => CursorIcon::NotAllowed,
        Cursor::Grab => CursorIcon::Grab,
        Cursor::Grabbing => CursorIcon::Grabbing,
        Cursor::EResize => CursorIcon::EResize,
        Cursor::NResize => CursorIcon::NResize,
        Cursor::NeResize => CursorIcon::NeResize,
        Cursor::NwResize => CursorIcon::NwResize,
        Cursor::SResize => CursorIcon::SResize,
        Cursor::SeResize => CursorIcon::SeResize,
        Cursor::SwResize => CursorIcon::SwResize,
        Cursor::WResize => CursorIcon::WResize,
        Cursor::EwResize => CursorIcon::EwResize,
        Cursor::NsResize => CursorIcon::NsResize,
        Cursor::NeswResize => CursorIcon::NeswResize,
        Cursor::NwseResize => CursorIcon::NwseResize,
        Cursor::ColResize => CursorIcon::ColResize,
        Cursor::RowResize => CursorIcon::RowResize,
        Cursor::AllScroll => CursorIcon::AllScroll,
        Cursor::ZoomIn => CursorIcon::ZoomIn,
        Cursor::ZoomOut => CursorIcon::ZoomOut,
    })
}

#[cfg(test)]
mod tests {
    use std::{path::Path, sync::Arc};

    use squark_compositor::{Compositor, FrameMailbox, HitSnapshots, Painter};
    use squark_layout::StaticDocument;
    use squark_types::{
        Size,
        engine::{DocumentEngine, FrameInput},
        render::{FramePainter, resource_channel},
    };
    use winit::event::{Force, Modifiers as WinitModifiers};

    use super::*;

    fn snapshot() -> Arc<HitSnapshot> {
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/runtime.sqpak");
        let pack = crate::document::open_pack(&fixture).unwrap();
        let (sender, _updates) = resource_channel();
        let url = url::Url::parse("http://squark.invalid/index.html").unwrap();
        let mut engine = StaticDocument::from_pack(&pack, url, sender.clone()).unwrap();
        let viewport = Viewport {
            css: Size {
                width: 40.0,
                height: 30.0,
            },
            device: [80, 60],
            scale: 2.0,
            epoch: 5,
        };
        let tick = crate::static_tick(viewport);
        engine
            .update_layout(&FrameInput {
                tick,
                media: Default::default(),
                scroll: Default::default(),
            })
            .unwrap();
        let mailbox = Arc::new(FrameMailbox::default());
        let mut painter = Painter::new(Arc::clone(&mailbox), sender.clone());
        let (tree, mirror) = engine.snapshot();
        painter.commit(&tick, tree, mirror).unwrap();
        let hits = Arc::new(HitSnapshots::default());
        let mut compositor = Compositor::new(mailbox, Arc::clone(&hits));
        compositor
            .adopt_latest(viewport, sender.watermark())
            .unwrap();
        compositor.mark_presented();
        hits.latest().unwrap()
    }

    #[test]
    fn native_mouse_and_wheel_use_the_presented_epoch_and_css_coordinates() {
        let mut router = InputRouter::default();
        let view = snapshot();
        let now = Instant::now();
        let device_id = DeviceId::dummy();
        let mut send = |event| router.event(&event, now, Some(view.viewport), Some(&view));
        let mods = WinitModifiers::from(ModifiersState::SHIFT | ModifiersState::CONTROL);
        assert!(send(WindowEvent::ModifiersChanged(mods)).is_none());
        let position = PhysicalPosition::new(8.0, 6.0);
        let css = Point { x: 4.0, y: 3.0 };
        let hit = hit_test(&view, css).unwrap();

        let task = send(WindowEvent::CursorMoved {
            device_id,
            position,
        })
        .unwrap();
        assert_eq!(task.target, hit.target);
        assert_eq!(task.related, NodeId::NONE);
        assert_eq!(task.request, 1);
        assert_eq!(task.time, now);
        assert!(task.modifiers.shift && task.modifiers.ctrl);
        assert_eq!(
            task.input,
            Input::Pointer {
                kind: PointerKind::Move,
                boundary_update: false,
                click_count: None,
                id: 1,
                device: PointerType::Mouse,
                primary: true,
                css,
                pressure: 0.0,
                button: -1,
                buttons: 0,
                samples: Box::new([]),
            }
        );

        let task = send(WindowEvent::MouseInput {
            device_id,
            state: ElementState::Pressed,
            button: MouseButton::Right,
        })
        .unwrap();
        assert!(matches!(
            task.input,
            Input::Pointer {
                kind: PointerKind::Down,
                button: 2,
                buttons: 2,
                ..
            }
        ));
        let task = send(WindowEvent::MouseWheel {
            device_id,
            phase: TouchPhase::Moved,
            delta: MouseScrollDelta::PixelDelta(PhysicalPosition::new(6.0, -10.0)),
        })
        .unwrap();
        assert_eq!(
            task.input,
            Input::Wheel {
                css,
                delta: [-3.0, 5.0, 0.0],
                mode: DeltaMode::Pixel
            }
        );
        let task = send(WindowEvent::CursorLeft { device_id }).unwrap();
        assert_eq!(task.target, NodeId::NONE);
        assert_eq!(task.related, hit.target);
        let task = send(WindowEvent::MouseInput {
            device_id,
            state: ElementState::Released,
            button: MouseButton::Right,
        })
        .unwrap();
        assert!(matches!(
            task.input,
            Input::Pointer {
                kind: PointerKind::Up,
                buttons: 0,
                ..
            }
        ));

        let viewport = Viewport {
            epoch: 6,
            ..view.viewport
        };
        let task = router
            .event(
                &WindowEvent::CursorMoved {
                    device_id,
                    position,
                },
                now,
                Some(viewport),
                Some(&view),
            )
            .unwrap();
        assert_eq!(task.target, NodeId::NONE);
    }

    #[test]
    fn test_control_click_count_is_copied_into_pointer_input() {
        let view = snapshot();
        let mut router = InputRouter::default();
        let task = router.synthetic(
            crate::control::Request::Pointer(crate::control::Pointer {
                kind: crate::control::PointerKind::Up,
                x: 4.0,
                y: 3.0,
                button: 0,
                buttons: 0,
                modifiers: 0,
                pointer_id: 17,
                click_count: Some(2),
            }),
            Instant::now(),
            view.viewport,
            Some(&view),
        );
        assert!(matches!(
            task.input,
            Input::Pointer {
                id: 17,
                click_count: Some(2),
                ..
            }
        ));
    }

    #[test]
    fn native_touch_ids_are_stable_and_only_the_first_contact_is_primary() {
        let mut router = InputRouter::default();
        let view = snapshot();
        let now = Instant::now();
        let mut send = |id, phase| {
            router
                .event(
                    &WindowEvent::Touch(Touch {
                        device_id: DeviceId::dummy(),
                        phase,
                        location: PhysicalPosition::new(8.0, 6.0),
                        force: Some(Force::Normalized(0.75)),
                        id,
                    }),
                    now,
                    Some(view.viewport),
                    Some(&view),
                )
                .unwrap()
                .input
        };
        let first = u64::MAX;
        assert!(matches!(
            send(first, TouchPhase::Started),
            Input::Pointer {
                id: 2,
                primary: true,
                pressure: 0.75,
                ..
            }
        ));
        assert!(matches!(
            send(1, TouchPhase::Started),
            Input::Pointer {
                id: 3,
                primary: false,
                ..
            }
        ));
        assert!(matches!(
            send(first, TouchPhase::Ended),
            Input::Pointer {
                id: 2,
                kind: PointerKind::Up,
                buttons: 0,
                ..
            }
        ));
        assert!(matches!(
            send(1, TouchPhase::Moved),
            Input::Pointer {
                id: 3,
                primary: false,
                ..
            }
        ));
        assert!(matches!(
            send(1, TouchPhase::Cancelled),
            Input::Pointer {
                id: 3,
                kind: PointerKind::Cancel,
                pressure: 0.0,
                ..
            }
        ));
        assert!(matches!(
            send(first, TouchPhase::Started),
            Input::Pointer {
                id: 4,
                primary: true,
                ..
            }
        ));
    }
    #[test]
    fn cursor_follows_the_presented_hit_and_resets_outside_it() {
        let mut snapshot = snapshot();
        let viewport = snapshot.viewport;
        let point = Point { x: 4.0, y: 3.0 };
        let target = hit_test(&snapshot, point).unwrap().target;
        for region in Arc::make_mut(&mut Arc::get_mut(&mut snapshot).unwrap().regions.regions) {
            if region.target == target {
                region.cursor = Cursor::Pointer;
            }
        }

        let mut router = InputRouter::default();
        let device_id = DeviceId::dummy();
        let now = Instant::now();
        router.event(
            &WindowEvent::CursorMoved {
                device_id,
                position: PhysicalPosition::new(8.0, 6.0),
            },
            now,
            Some(viewport),
            Some(&snapshot),
        );
        assert_eq!(
            router.cursor_at(Some(viewport), Some(&snapshot)),
            Cursor::Pointer
        );
        assert_eq!(cursor_icon(Cursor::Pointer), Some(CursorIcon::Pointer));
        assert_eq!(cursor_icon(Cursor::None), None);
        router.event(
            &WindowEvent::CursorLeft { device_id },
            now,
            Some(viewport),
            Some(&snapshot),
        );
        assert_eq!(
            router.cursor_at(Some(viewport), Some(&snapshot)),
            Cursor::Auto
        );
    }

    #[test]
    fn native_click_through_the_document_channel_commits_the_js_handler() {
        use squark_js::document::DocumentTask;
        use squark_paint::Primitive;
        use std::sync::mpsc;

        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/runtime-js.sqpak");
        let pack = Arc::new(crate::document::open_pack(&path).unwrap());
        crate::document::initialize_v8();
        let (resources, _updates) = resource_channel();
        let watermark = resources.clone();
        let mailbox = Arc::new(FrameMailbox::default());
        let painter = Painter::new(Arc::clone(&mailbox), resources.clone());
        let viewport = Viewport {
            css: Size {
                width: 40.0,
                height: 30.0,
            },
            device: [60, 45],
            scale: 1.5,
            epoch: 1,
        };
        let first = crate::static_tick(viewport);
        let (tasks, receiver) = mpsc::channel();
        let host_service = crate::services::HostService::start(
            tasks.clone(),
            crate::services::ClipboardMode::Memory(crate::services::MemoryClipboard::default()),
            crate::services::FileDialogMode::Queued(crate::services::FileDialogQueue::default()),
            crate::services::FullscreenMode::Recorded(Default::default()),
                    crate::native::RecordedWindow::default().sink(),
        )
        .unwrap();
        let mut runner = crate::document::runner(
            &pack,
            painter,
            crate::document::DocumentChannels {
                resources,
                host_requests: host_service.sender(),
                canvas: None,
                native: None,
            },
            first,
            false,
            crate::document::DocumentControl {
                shutdown: squark_js::DocumentShutdown::default(),
                frame_demand: Default::default(),
            },
            move || first.vsync,
        )
        .unwrap();
        runner.frame(first).unwrap();

        let hits = Arc::new(HitSnapshots::default());
        let mut compositor = Compositor::new(Arc::clone(&mailbox), Arc::clone(&hits));
        compositor
            .adopt_latest(viewport, watermark.watermark())
            .unwrap();
        compositor.mark_presented();
        let hit = hits.latest().unwrap();

        let mut router = InputRouter::default();
        let device_id = DeviceId::dummy();
        for event in [
            WindowEvent::CursorMoved {
                device_id,
                position: PhysicalPosition::new(9.0, 6.0),
            },
            WindowEvent::MouseInput {
                device_id,
                state: ElementState::Pressed,
                button: MouseButton::Left,
            },
            WindowEvent::MouseInput {
                device_id,
                state: ElementState::Released,
                button: MouseButton::Left,
            },
        ] {
            let task = router
                .event(&event, first.vsync, Some(viewport), Some(&hit))
                .unwrap();
            assert!(!task.target.is_none());
            tasks
                .send(DocumentTask::Input { task, reply: None })
                .unwrap();
        }
        let tick = squark_types::render::FrameTick {
            sequence: 2,
            ..crate::static_tick(viewport)
        };
        tasks.send(DocumentTask::Frame(tick)).unwrap();
        runner
            .run(receiver, |_| tasks.send(DocumentTask::Shutdown).unwrap())
            .unwrap();

        let frame = mailbox.take_latest().unwrap();
        let Primitive::RoundedRect { color, .. } = frame.frame.display_list[0].primitive else {
            panic!("expected a canvas fill");
        };
        assert_eq!(
            color.rgba,
            [221.0 / 255.0, 102.0 / 255.0, 34.0 / 255.0, 1.0]
        );
    }
}
