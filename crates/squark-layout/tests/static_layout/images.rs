// SPDX-License-Identifier: MIT
use std::{
    io::{BufRead, BufReader, Write},
    net::TcpListener,
    sync::mpsc,
    time::{Duration, Instant},
};

use squark_layout::StaticDocument;
use squark_types::{
    NodeId, Size,
    engine::{DecodedBatch, DocumentEngine, FrameInput},
    fragment::FragmentContent,
    journal::{Opcode, Record},
    render::{FrameDemand, FrameTick, ResourceUpdate, Viewport, resource_channel},
};

use super::{BODY, Fixture, VIEWPORT, find};

const SVG: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" width="48" height="32"><rect width="48" height="32" fill="red"/></svg>"#;
const DATA_SVG: &str =
    "data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' width='20' height='12'/%3E";

fn record(op: Opcode, a: u32, b: u32, c: u32) -> Record {
    Record {
        op,
        flags: 0,
        a,
        b,
        c,
    }
}

fn flush(engine: &mut StaticDocument<'_>, records: &[Record], strings: &[&[u16]]) {
    engine
        .flush(DecodedBatch {
            records,
            strings,
            staged_bytes: &[],
        })
        .unwrap();
}

fn frame() -> FrameInput<'static> {
    let now = Instant::now();
    FrameInput {
        tick: FrameTick {
            sequence: 1,
            vsync: now,
            present_at: now,
            commit_deadline: now,
            viewport: Viewport {
                css: VIEWPORT,
                device: [640, 480],
                scale: 1.0,
                epoch: 1,
            },
        },
        media: Default::default(),
        scroll: Default::default(),
    }
}

#[test]
fn starts_allowlisted_requests_for_detached_images_and_uploads_on_attach() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let url = format!("http://{address}/image.svg");
    let denied = format!("http://localhost:{}/denied.svg", address.port());
    let mut fixture = Fixture::new("");
    let detached = fixture.element(BODY, "img", &[("src", &url)]);
    fixture
        .journal
        .record(Opcode::Remove, 0, BODY, detached, 0)
        .unwrap();
    let blocked = fixture.element(
        BODY,
        "img",
        &[("src", &denied), ("width", "32"), ("height", "24")],
    );
    let manifest = serde_json::json!({"network": {"allow": [address.to_string()]}});
    let pack = fixture.pack_with_manifest(Some(manifest.to_string().as_bytes()));
    let (publisher, updates) = resource_channel();
    let (wake, wakes) = mpsc::channel();
    let mut engine =
        StaticDocument::from_pack(&pack, "http://squark.invalid/".parse().unwrap(), publisher)
            .unwrap()
            .with_image_frame_demand(FrameDemand::new(move || {
                let _ = wake.send(());
            }));

    std::thread::scope(|scope| {
        let response = scope.spawn(|| {
            let expires = Instant::now() + Duration::from_secs(4);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < expires, "no image request arrived");
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("{error}"),
                }
            };
            // On macOS/BSD, sockets accepted from a non-blocking listener inherit
            // O_NONBLOCK; switch back to blocking so read_line obeys the timeout
            // instead of failing immediately with WouldBlock (EAGAIN).
            stream.set_nonblocking(false).unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
            let mut request = String::new();
            BufReader::new(&stream).read_line(&mut request).unwrap();
            assert_eq!(request, "GET /image.svg HTTP/1.1\r\n");
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: image/svg+xml\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{SVG}",
                SVG.len(),
            ).unwrap();
        });

        let input = frame();
        engine.update_layout(&input).unwrap();
        let events = engine.take_image_events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].target, NodeId::from_raw(blocked));
        assert_eq!(events[0].id, 1);
        assert_eq!(events[0].natural_size, None);
        assert!(matches!(
            find(engine.snapshot().0, blocked).content,
            FragmentContent::Svg(_)
        ));

        wakes
            .recv_timeout(Duration::from_secs(4))
            .expect("image completion wakes the document");
        response.join().unwrap();
        engine.update_layout(&input).unwrap();
        let events = engine.take_image_events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].target, NodeId::from_raw(detached));
        assert_eq!(events[0].id, 1);
        assert_eq!(events[0].natural_size, Some([48, 32]));
        assert!(
            !engine
                .snapshot()
                .0
                .nodes
                .values()
                .any(|node| node.owner.raw() == detached)
        );

        flush(
            &mut engine,
            &[record(Opcode::Insert, BODY, detached, 0)],
            &[],
        );
        engine.update_layout(&input).unwrap();
        let image = find(engine.snapshot().0, detached);
        assert_eq!(
            image.border_box.size,
            Size {
                width: 48.0,
                height: 32.0
            }
        );
        let FragmentContent::Image { key, .. } = image.content else {
            panic!("expected the decoded image");
        };
        let bitmap = updates
            .try_iter()
            .find_map(|message| match message.update {
                ResourceUpdate::PutImage {
                    key: candidate,
                    bitmap,
                } if candidate == key => Some(bitmap),
                _ => None,
            })
            .expect("the presented frame needs the image's bitmap");
        assert_eq!(bitmap.size, [48, 32]);
        assert_eq!(&bitmap.pixels[..4], &[255, 0, 0, 255]);
        assert!(engine.take_image_events().is_empty());
        assert!(listener.accept().is_err(), "the denied host was contacted");
    });
}

#[test]
fn counts_every_src_write_and_discards_superseded_completions() {
    let mut fixture = Fixture::new("");
    let img = fixture.element(BODY, "img", &[("src", DATA_SVG)]);
    let pack = fixture.pack();
    let (publisher, _updates) = resource_channel();
    let mut engine =
        StaticDocument::from_pack(&pack, "http://squark.invalid/".parse().unwrap(), publisher)
            .unwrap();
    engine.update_layout(&frame()).unwrap();
    let initial = engine.take_image_events();
    assert_eq!(initial.len(), 1);
    assert_eq!(
        (initial[0].id, initial[0].natural_size),
        (1, Some([20, 12]))
    );

    let src: Vec<_> = "src".encode_utf16().collect();
    let good: Vec<_> = DATA_SVG.encode_utf16().collect();
    let bad: Vec<_> = "data:image/png,corrupt".encode_utf16().collect();
    flush(
        &mut engine,
        &[
            record(Opcode::DefineAtom, 1000, 0, 2),
            record(Opcode::SetAttr, img, 1000, 1),
            record(Opcode::RemoveAttr, img, 1000, 0),
            record(Opcode::SetAttr, img, 1000, 2),
        ],
        &[&src, &good, &bad],
    );
    let latest = engine.take_image_events();
    assert_eq!(latest.len(), 1);
    assert_eq!((latest[0].id, latest[0].natural_size), (4, None));
}

#[test]
fn resolves_blob_urls_for_image_elements_and_css_backgrounds() {
    use squark_types::fragment::BackgroundImage;
    use std::sync::Arc;

    let mut fixture = Fixture::new(
        r#"#background { width: 48px; height: 32px; background-image: url("blob:https://example.test/photo") }"#,
    );
    let image = fixture.element(BODY, "img", &[("src", "blob:https://example.test/photo")]);
    let data = fixture.element(BODY, "img", &[("src", DATA_SVG)]);
    let background = fixture.element(BODY, "div", &[("id", "background")]);
    let pack = fixture.pack();
    let (publisher, updates) = resource_channel();
    let mut engine =
        StaticDocument::from_pack(&pack, "https://example.test/".parse().unwrap(), publisher)
            .unwrap()
            .with_blob_resolver(Arc::new(|url| {
                (url == "blob:https://example.test/photo")
                    .then(|| (Arc::from(SVG.as_bytes()), "image/svg+xml".into()))
            }));
    engine.update_layout(&frame()).unwrap();
    let sizes: std::collections::BTreeMap<_, _> = engine
        .take_image_events()
        .into_iter()
        .map(|event| (event.target.raw(), event.natural_size))
        .collect();
    assert_eq!(sizes[&image], Some([48, 32]));
    assert_eq!(sizes[&data], Some([20, 12]));
    let tree = engine.snapshot().0;
    let image_box = find(tree, image);
    let data_box = find(tree, data);
    assert_eq!(
        image_box.border_box.size,
        Size {
            width: 48.0,
            height: 32.0
        }
    );
    assert_eq!(
        data_box.border_box.size,
        Size {
            width: 20.0,
            height: 12.0
        }
    );
    let FragmentContent::Image { key, .. } = image_box.content else {
        panic!("expected decoded blob in image fragment");
    };
    assert_eq!(
        find(tree, background).style.box_paint.backgrounds[0].image,
        BackgroundImage::Image(key)
    );
    assert!(updates.try_iter().any(|update| matches!(update.update,
        ResourceUpdate::PutImage { key: uploaded, bitmap } if uploaded == key && bitmap.size == [48, 32]
    )));
}

#[test]
fn rechecks_cached_blob_urls_for_each_image_load() {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    const BLOB_URL: &str = "blob:https://example.test/photo";
    let mut fixture = Fixture::new("");
    let original = fixture.element(BODY, "img", &[("src", BLOB_URL)]);
    let cached = fixture.element(BODY, "img", &[]);
    let revoked = fixture.element(BODY, "img", &[("alt", "photo")]);
    let pack = fixture.pack();
    let (publisher, updates) = resource_channel();
    let live = Arc::new(AtomicBool::new(true));
    let mut engine =
        StaticDocument::from_pack(&pack, "https://example.test/".parse().unwrap(), publisher)
            .unwrap()
            .with_blob_resolver(Arc::new({
                let live = Arc::clone(&live);
                move |url| {
                    (url == BLOB_URL && live.load(Ordering::Relaxed))
                        .then(|| (Arc::from(SVG.as_bytes()), "image/svg+xml".into()))
                }
            }));
    let input = frame();
    engine.update_layout(&input).unwrap();
    assert_eq!(engine.take_image_events()[0].natural_size, Some([48, 32]));
    let FragmentContent::Image { key, .. } = find(engine.snapshot().0, original).content else {
        panic!("expected the initial blob image");
    };
    assert!(updates.try_iter().any(|message| matches!(message.update,
        ResourceUpdate::PutImage { key: uploaded, .. } if uploaded == key
    )));

    let name: Vec<_> = "src".encode_utf16().collect();
    let blob: Vec<_> = BLOB_URL.encode_utf16().collect();
    let data: Vec<_> = DATA_SVG.encode_utf16().collect();
    flush(
        &mut engine,
        &[
            record(Opcode::DefineAtom, 1000, 0, 2),
            record(Opcode::SetAttr, cached, 1000, 1),
        ],
        &[&name, &blob],
    );
    assert_eq!(engine.take_image_events()[0].natural_size, Some([48, 32]));

    live.store(false, Ordering::Relaxed);
    flush(
        &mut engine,
        &[record(Opcode::SetAttr, revoked, 1000, 0)],
        &[&blob],
    );
    let events = engine.take_image_events();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].target, NodeId::from_raw(revoked));
    assert_eq!(events[0].natural_size, None);
    engine.update_layout(&input).unwrap();
    let tree = engine.snapshot().0;
    for owner in [original, cached] {
        assert!(matches!(
            find(tree, owner).content,
            FragmentContent::Image { key: retained, .. } if retained == key
        ));
    }
    assert!(matches!(
        find(tree, revoked).content,
        FragmentContent::Svg(_)
    ));
    assert!(
        !updates
            .try_iter()
            .any(|message| matches!(message.update, ResourceUpdate::PutImage { .. }))
    );

    flush(
        &mut engine,
        &[record(Opcode::SetAttr, revoked, 1000, 0)],
        &[&data],
    );
    assert_eq!(engine.take_image_events()[0].natural_size, Some([20, 12]));
    engine.update_layout(&input).unwrap();
    assert!(matches!(
        find(engine.snapshot().0, revoked).content,
        FragmentContent::Image { key: replacement, .. } if replacement != key
    ));
}
