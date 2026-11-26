// SPDX-License-Identifier: MIT

use squark_js::CanvasBackend;
use squark_types::render::ResourceUpdate;

use super::*;

struct Canvas {
    journal: Rc<RefCell<JournalLog>>,
    trace: Trace,
}

impl CanvasBackend for Canvas {
    fn readback(
        &mut self,
        canvas: NodeId,
        origin: [i32; 2],
        size: [u32; 2],
    ) -> Result<Vec<u8>, String> {
        let log = self.journal.borrow();
        assert_eq!(log.canvases.last().unwrap().0, canvas);
        assert_eq!(origin, [-1, 0]);
        assert_eq!(size, [2, 1]);
        self.trace
            .borrow_mut()
            .push(format!("readback:{}", log.canvases.len()));
        Ok(vec![0, 0, 0, 0, 10, 20, 30, 255])
    }

    fn encode(
        &mut self,
        canvas: NodeId,
        mime: &str,
        quality: Option<f64>,
    ) -> Result<Vec<u8>, String> {
        let log = self.journal.borrow();
        assert_eq!(log.canvases.last().unwrap().0, canvas);
        self.trace
            .borrow_mut()
            .push(format!("encode:{}:{mime}:{quality:?}", log.canvases.len()));
        if mime == "image/webp" {
            return Err("missing WebP fixture".into());
        }
        Ok(vec![42, 43, 44])
    }
}

#[test]
fn canvas_css_uses_stylo_without_flushing_pending_draws() {
    support::initialize_v8();
    let origin = Instant::now();
    let pack =
        Pack::open(&File::open(key("tests/fixtures/pack/basic.appended.bin")).unwrap()).unwrap();
    let (resources, updates) = resource_channel();
    let engine = StaticDocument::from_pack(&pack, URL.parse().unwrap(), resources).unwrap();
    let mut runner = DocumentRunner::new(
        options(origin, true),
        pack.entry(DOCUMENT_PATH).unwrap().bytes,
        sources(|path| (path == "/assets/main.js").then(|| ("text/javascript".into(), vec![]))),
        Box::new(engine),
        Box::new(Paint(Trace::default())),
        Instant::now,
    )
    .unwrap();
    runner
        .eval(
            r#"
        globalThis.canvas = document.createElement("canvas");
        canvas.style.color = "rgb(7, 128, 220)";
        document.body.append(canvas);
        globalThis.ctx = canvas.getContext("2d");
    "#,
        )
        .unwrap();
    runner.frame(tick(origin, 1, 0)).unwrap();
    updates.try_iter().for_each(drop);
    assert_eq!(
        runner
            .eval(
                r#"
        ctx.fillStyle = "currentColor";
        ctx.fillStyle = "rgb(invalid)";
        ctx.strokeStyle = "color-mix(in srgb, red, blue)";
        ctx.filter = " NONE ";
        ctx.fillRect(0, 0, 1, 1);
        [ctx.fillStyle, ctx.strokeStyle, ctx.filter]
    "#,
            )
            .unwrap(),
        r##"["#0780dc","#800080"," NONE "]"##
    );
    assert!(matches!(updates.try_recv(), Err(mpsc::TryRecvError::Empty)));
    runner.frame(tick(origin, 2, 16)).unwrap();
    let stream = updates
        .try_iter()
        .find_map(|message| match message.update {
            ResourceUpdate::SubmitCanvas { stream, .. } => Some(stream),
            _ => None,
        })
        .expect("canvas draw should reach the resource queue");
    assert_eq!(ops(&stream), [20, 18, 20, 19, 28, 14]);

    assert_eq!(
        runner
            .eval(
                r#"
        ctx.filter = "invert(93%) hue-rotate(180deg)";
        ctx.filter = "blur(invalid)";
        ctx.filter
    "#,
            )
            .unwrap(),
        r#""invert(93%) hue-rotate(180deg)""#
    );
    assert!(matches!(updates.try_recv(), Err(mpsc::TryRecvError::Empty)));
}

fn ops(bytes: &[u8]) -> Vec<u8> {
    let word = |offset| u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize;
    assert_eq!(&bytes[..4], b"JCV1");
    assert_eq!(word(4), bytes.len());
    let mut codes = Vec::new();
    let mut offset = 12;
    while offset < bytes.len() {
        codes.push(bytes[offset]);
        assert_eq!(&bytes[offset + 1..offset + 4], &[0; 3]);
        let size = word(offset + 4);
        offset += 8 + size.next_multiple_of(8);
    }
    assert_eq!(offset, bytes.len());
    assert_eq!(codes.len(), word(8));
    codes
}

#[test]
fn canvas_readback_encoding_and_staging_are_ordered_in_the_v8_runner() {
    support::initialize_v8();
    let trace = Trace::default();
    let engine = Engine::new(trace.clone());
    let journal = engine.journal.clone();
    let origin = Instant::now();
    let mut runner = DocumentRunner::new(
        options(origin, true),
        &fixture(&[]),
        sources(|_| None).with_canvas_backend(Canvas {
            journal: journal.clone(),
            trace: trace.clone(),
        }),
        Box::new(engine),
        Box::new(Paint(trace.clone())),
        move || origin,
    )
    .unwrap();
    assert_eq!(
        runner
            .eval(
                r#"
        globalThis.canvas = document.createElement("canvas");
        document.body.appendChild(canvas);
        canvas.id = "visible";
        globalThis.ctx = canvas.getContext("2d", {willReadFrequently: true});
        globalThis.cached = document.createElement("canvas");
        cached.id = "cached";
        globalThis.offscreen = cached.getContext("2d");
        offscreen.fillStyle = "currentColor";
        offscreen.fillRect(1, 2, 3, 4);
        ctx.drawImage(cached, 5, 6);
        offscreen.clearRect(0, 0, 300, 150);
        ctx.drawImage(cached, 4, 5, 6, 7);
        const image = ctx.getImageData(-1, 0, 2, 1);
        [ctx.canvas === canvas, offscreen.fillStyle, image instanceof ImageData,
            image.width, image.height, ...image.data]
    "#
            )
            .unwrap(),
        "[true,\"#0a141e\",true,2,1,0,0,0,0,10,20,30,255]"
    );
    {
        let log = journal.borrow();
        let visible = log.ids["visible"];
        let cache = log.ids["cached"];
        assert_eq!(
            log.canvases
                .iter()
                .map(|(owner, bytes)| (*owner, ops(bytes)))
                .collect::<Vec<_>>(),
            vec![
                (cache, vec![20, 18, 14]),
                (visible, vec![23]),
                (cache, vec![16]),
                (visible, vec![23])
            ]
        );
    }
    assert_eq!(
        runner
            .eval(
                r#"
        ctx.filter = "invert(93%) hue-rotate(180deg)";
        ctx.filter = "invalid";
        ctx.font = " 12PX    SERIF ";
        ctx.font = "invalid";
        ctx.fillText("a", 4, 5);
        [ctx.filter, ctx.font, ctx.measureText("\ud800T").width, canvas.toDataURL("unknown")]
    "#
            )
            .unwrap(),
        r#"["invert(93%) hue-rotate(180deg)","12px serif",14,"data:image/png;base64,Kiss"]"#
    );
    assert_eq!(
        ops(&journal.borrow().canvases.last().unwrap().1),
        vec![27, 26]
    );
    assert_eq!(runner.eval(r#"
        globalThis.result = [];
        ctx.strokeRect(0, 0, 2, 2);
        canvas.toBlob(async blob => result.push([blob instanceof Blob, blob.type, ...new Uint8Array(await blob.arrayBuffer())]),
            "IMAGE/JPEG", .75);
        canvas.toBlob(blob => result.push(blob), "image/webp");
        result.length
    "#).unwrap(), "0");
    runner.run_ready().unwrap();
    assert_eq!(
        runner.eval("result").unwrap(),
        r#"[[true,"image/jpeg",42,43,44],null]"#
    );
    assert!(trace.borrow().iter().any(|event| event == "readback:4"));
    assert!(
        trace
            .borrow()
            .iter()
            .any(|event| event == "encode:5:image/png:None")
    );
    assert!(
        trace
            .borrow()
            .iter()
            .any(|event| event == "encode:6:image/jpeg:Some(0.75)")
    );

    runner.eval("ctx.fillRect(0, 0, 1, 1);").unwrap();
    assert_eq!(journal.borrow().canvases.len(), 6);
    runner.frame(tick(origin, 1, 0)).unwrap();
    assert_eq!(ops(&journal.borrow().canvases.last().unwrap().1), vec![14]);
}

#[test]
fn image_bitmap_snapshots_canvases_images_blobs_and_previous_bitmaps() {
    struct Pixels;
    impl CanvasBackend for Pixels {
        fn readback(
            &mut self,
            _: NodeId,
            origin: [i32; 2],
            size: [u32; 2],
        ) -> Result<Vec<u8>, String> {
            assert_eq!(origin, [0, 0]);
            assert_eq!(size, [2, 1]);
            Ok(vec![255, 0, 0, 255, 0, 255, 0, 255])
        }
    }
    support::initialize_v8();
    let origin = Instant::now();
    let pack = Arc::new(
        Pack::open(&File::open(key("tests/fixtures/pack/basic.appended.bin")).unwrap()).unwrap(),
    );
    let (resources, updates) = resource_channel();
    let engine = StaticDocument::from_pack(&pack, URL.parse().unwrap(), resources).unwrap();
    let mut runner = DocumentRunner::new(
        options(origin, true),
        pack.entry(DOCUMENT_PATH).unwrap().bytes,
        sources({
            let pack = Arc::clone(&pack);
            move |path| {
                let entry = pack.entry(path)?;
                Some((entry.mime.into(), entry.bytes.to_vec()))
            }
        })
        .with_canvas_backend(Pixels),
        Box::new(engine),
        Box::new(Paint(Trace::default())),
        Instant::now,
    )
    .unwrap();
    runner.eval(r#"
        globalThis.bitmaps = null;
        globalThis.bitmapStep = "canvas";
        (async () => {
          const canvas = document.createElement("canvas");
          canvas.width = 2; canvas.height = 1;
          const fromCanvas = await createImageBitmap(canvas, {resizeWidth:4, resizeQuality:"pixelated"});
          bitmapStep = "bitmap";
          const fromBitmap = await createImageBitmap(fromCanvas, -1, 0, 2, 1);
          bitmapStep = "blob";
          const blob = await (await fetch("/images/dot.png")).blob();
          const fromBlob = await createImageBitmap(blob, {resizeWidth:3, resizeHeight:2});
          bitmapStep = "image load";
          const image = new Image();
          image.src = "/images/dot.png";
          await image.decode();
          bitmapStep = "image snapshot";
          const fromImage = await createImageBitmap(image);
          const ctx = canvas.getContext("2d");
          ctx.drawImage(fromBitmap, 0, 0);
          const dimensions = [fromCanvas, fromBitmap, fromBlob, fromImage].map(bitmap => [bitmap.width, bitmap.height]);
          fromBitmap.close();
          let closed;
          try { ctx.drawImage(fromBitmap, 0, 0); } catch (error) { closed = error.name; }
          const empty = await createImageBitmap(image, 0, 0, 0, 1).catch(error => error.name);
          const brands = [canvas, image, fromCanvas].map(source => Object.prototype.toString.call(source));
          bitmaps = [fromBlob instanceof ImageBitmap, dimensions, fromBitmap.width, fromBitmap.height, closed, empty, brands];
        })().catch(error => bitmaps = [String(error), error.stack]);
    "#).unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut sequence = 1;
    let actual = loop {
        runner.frame(tick(origin, sequence, sequence * 16)).unwrap();
        runner.run_ready().unwrap();
        let printed = runner.eval("bitmaps").unwrap();
        if printed != "null" {
            break printed;
        }
        assert!(
            Instant::now() < deadline,
            "bitmap snapshot did not settle at {}",
            runner.eval("bitmapStep").unwrap()
        );
        std::thread::sleep(Duration::from_millis(2));
        sequence += 1;
    };
    assert_eq!(
        actual,
        concat!(
            r#"[true,[[4,2],[2,1],[3,2],[1,1]],0,0,"InvalidStateError","RangeError","#,
            r#"["[object HTMLCanvasElement]","[object HTMLImageElement]","[object ImageBitmap]"]]"#,
        )
    );
    runner.eval("gc()").unwrap();
    runner
        .frame(tick(origin, sequence + 1, (sequence + 1) * 16))
        .unwrap();
    assert!(updates.try_iter().any(|message| matches!(
        message.update, ResourceUpdate::SubmitCanvas { stream, .. } if ops(&stream) == [23]
            && stream[20..24] == [0; 4] && stream[24..28] != [0; 4]
    )));
}
