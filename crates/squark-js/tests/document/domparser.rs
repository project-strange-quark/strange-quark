// SPDX-License-Identifier: MIT

use std::process::Command;

use super::*;

pub(super) fn open_basic_pack() -> Arc<Pack> {
    Arc::new(
        Pack::open(&File::open(key("tests/fixtures/pack/basic.appended.bin")).unwrap()).unwrap(),
    )
}

pub(super) fn real_engine_runner<'pack>(
    pack: &'pack Arc<Pack>,
    script: &str,
) -> (
    DocumentRunner<'pack>,
    Arc<FrameMailbox>,
    mpsc::Receiver<squark_types::render::ResourceMessage>,
    Instant,
) {
    support::initialize_v8();
    let origin = Instant::now();
    let bytes = pack.entry(DOCUMENT_PATH).unwrap().bytes;
    let (resources, updates) = resource_channel();
    let mailbox = Arc::new(FrameMailbox::default());
    let engine = StaticDocument::from_pack(pack, URL.parse().unwrap(), resources.clone()).unwrap();
    let painter = Painter::new(mailbox.clone(), resources);
    let script_bytes = script.as_bytes().to_vec();
    let runner = DocumentRunner::new(
        options(origin, true),
        bytes,
        sources({
            let pack = pack.clone();
            move |path| {
                if path == "/assets/main.js" {
                    return Some(("text/javascript".into(), script_bytes.clone()));
                }
                let asset = pack.entry(path)?;
                Some((asset.mime.into(), asset.bytes.to_vec()))
            }
        }),
        Box::new(engine),
        Box::new(painter),
        Instant::now,
    )
    .unwrap();
    (runner, mailbox, updates, origin)
}

#[test]
fn domparser_handles_all_mime_types_normalize_svg_errors_and_exec_command() {
    let pack = open_basic_pack();
    let (mut runner, _mailbox, _updates, origin) = real_engine_runner(&pack, "");
    runner.frame(tick(origin, 1, 16)).unwrap();

    // 1. All 5 supported MIME types + TypeError for unsupported types.
    assert_eq!(
        runner
            .eval(
                r#"(() => {
                  const dp = new DOMParser();
                  const mimes = ["text/html", "text/xml", "application/xml", "application/xhtml+xml", "image/svg+xml"];
                  const ok = mimes.map(mime => {
                    const input = mime === "text/html"
                      ? "<!DOCTYPE html><html><body><div id='x'>Hi</div></body></html>"
                      : mime === "image/svg+xml"
                        ? '<svg xmlns="http://www.w3.org/2000/svg"><rect width="10" height="20"/></svg>'
                        : mime === "application/xhtml+xml"
                          ? '<html xmlns="http://www.w3.org/1999/xhtml"><body><p>X</p></body></html>'
                          : '<root><Item Attr="1">v</Item></root>';
                    const d = dp.parseFromString(input, mime);
                    return [d instanceof Document, d.defaultView === null, d.contentType === mime, d.querySelector("parsererror") === null];
                  });
                  let badType = false;
                  try { dp.parseFromString("<a/>", "text/plain"); } catch (e) { badType = e instanceof TypeError; }
                  let badCase = false;
                  try { dp.parseFromString("<a/>", "TEXT/HTML"); } catch (e) { badCase = e instanceof TypeError; }
                  return [ok.every(r => r.every(Boolean)), badType, badCase];
                })()"#
            )
            .unwrap(),
        "[true,true,true]"
    );

    // 2. Exact Excalidraw normalizeSVG sequence on real SVGs (with and without xmlns).
    assert_eq!(
        runner
            .eval(
                r##"(() => {
                  const SVG_NS = "http://www.w3.org/2000/svg";
                  const normalizeSVG = (SVGString) => {
                    const doc = new DOMParser().parseFromString(SVGString, "image/svg+xml");
                    const svg = doc.querySelector("svg");
                    const errorNode = doc.querySelector("parsererror");
                    if (errorNode || svg?.nodeName.toLowerCase() !== "svg") {
                      throw new Error("Invalid SVG");
                    }
                    if (!svg.hasAttribute("xmlns")) {
                      svg.setAttribute("xmlns", SVG_NS);
                    }
                    let width = svg.getAttribute("width");
                    let height = svg.getAttribute("height");
                    if (width?.includes("%") || width === "auto") width = null;
                    if (height?.includes("%") || height === "auto") height = null;
                    const viewBox = svg.getAttribute("viewBox");
                    if (!width || !height) {
                      width = width || "50";
                      height = height || "50";
                      if (viewBox) {
                        const match = viewBox.match(/\d+ +\d+ +(\d+(?:\.\d+)?) +(\d+(?:\.\d+)?)/);
                        if (match) [, width, height] = match;
                      }
                      svg.setAttribute("width", width);
                      svg.setAttribute("height", height);
                    }
                    if (!viewBox) {
                      svg.setAttribute("viewBox", `0 0 ${width} ${height}`);
                    }
                    return svg.outerHTML;
                  };

                  const withNs = normalizeSVG(
                    '<?xml version="1.0" encoding="UTF-8"?>\n<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 120 80"><circle cx="60" cy="40" r="30" fill="#e03131"/></svg>'
                  );
                  const reparsed1 = new DOMParser().parseFromString(withNs, "image/svg+xml");
                  const svg1 = reparsed1.querySelector("svg");

                  const withoutNs = normalizeSVG('<svg width="100%" height="auto" viewBox="0 0 64 32"><rect x="0" y="0" width="64" height="32"/></svg>');
                  const reparsed2 = new DOMParser().parseFromString(withoutNs, "image/svg+xml");
                  const svg2 = reparsed2.querySelector("svg");

                  return [
                    reparsed1.querySelector("parsererror") === null,
                    svg1.namespaceURI,
                    svg1.localName,
                    svg1.nodeName,
                    svg1.getAttribute("xmlns"),
                    svg1.getAttribute("width"),
                    svg1.getAttribute("height"),
                    svg1.getAttribute("viewBox"),
                    svg1.getAttribute("viewbox"),
                    reparsed1.querySelector("SVG") === null,
                    reparsed1.querySelector("circle").namespaceURI,
                    reparsed2.querySelector("parsererror") === null,
                    svg2.namespaceURI,
                    svg2.getAttribute("xmlns"),
                    svg2.getAttribute("width"),
                    svg2.getAttribute("height"),
                  ];
                })()"##
            )
            .unwrap(),
        r#"[true,"http://www.w3.org/2000/svg","svg","svg","http://www.w3.org/2000/svg","120","80","0 0 120 80",null,true,"http://www.w3.org/2000/svg",true,"http://www.w3.org/2000/svg","http://www.w3.org/2000/svg","64","32"]"#
    );

    // 3. Malformed SVG and XML produce <parsererror> with message, line and column.
    assert_eq!(
        runner
            .eval(
                r#"(() => {
                  const dp = new DOMParser();
                  const cases = [
                    '<svg xmlns="http://www.w3.org/2000/svg"><rect></svg>',
                    '<svg xmlns="http://www.w3.org/2000/svg"><rect/>',
                    '<svg xmlns="http://www.w3.org/2000/svg" width="10" width="20"/>',
                    '<svg xmlns="http://www.w3.org/2000/svg" bad:attr="1"/>',
                    '',
                  ];
                  return cases.map(src => {
                    const doc = dp.parseFromString(src, "image/svg+xml");
                    const pe = doc.querySelector("parsererror");
                    return pe !== null && /error on line \d+ at column \d+:/.test(pe.textContent);
                  });
                })()"#
            )
            .unwrap(),
        "[true,true,true,true,true]"
    );

    // 4. HTML parsing disables scripting (<script> does not run, <noscript> parses elements).
    assert_eq!(
        runner
            .eval(
                r#"(() => {
                  globalThis.__parsedScriptRan = false;
                  const doc = new DOMParser().parseFromString(
                    '<!DOCTYPE html><html><head><title>Parsed</title></head><body><div class="box"><noscript><p>fallback</p></noscript></div><script>globalThis.__parsedScriptRan = true;</script></body></html>',
                    'text/html'
                  );
                  const script = doc.querySelector("script");
                  document.body.appendChild(document.adoptNode(script));
                  return [
                    globalThis.__parsedScriptRan,
                    doc.title,
                    doc.compatMode,
                    doc.querySelector("noscript > p")?.textContent,
                    doc.querySelector(".box")?.nodeName,
                  ];
                })()"#
            )
            .unwrap(),
        r#"[false,"Parsed","CSS1Compat","fallback","DIV"]"#
    );

    // 5. document.execCommand and queryCommand* return false, and TextEncoder/TextDecoder round-trip SVG text.
    assert_eq!(
        runner
            .eval(
                r#"[
                  document.execCommand("copy"),
                  document.execCommand("cut"),
                  document.execCommand("paste"),
                  document.execCommand("bold"),
                  document.queryCommandSupported("copy"),
                  document.queryCommandEnabled("copy"),
                  new TextDecoder().decode(new TextEncoder().encode("<svg>✓</svg>")),
                ]"#
            )
            .unwrap(),
        r#"[false,false,false,false,false,false,"<svg>✓</svg>"]"#
    );
}

#[test]
fn adopting_parsed_nodes_journals_and_renders_them_in_the_live_document() {
    let pack = open_basic_pack();
    let (mut runner, mailbox, updates, origin) = real_engine_runner(&pack, "");
    runner.frame(tick(origin, 1, 16)).unwrap();

    // Parse HTML and SVG without touching the live document's journal.
    assert_eq!(
        runner
            .eval(
                r##"(() => {
                  const htmlDoc = new DOMParser().parseFromString(
                    '<!DOCTYPE html><div id="adopted-card" style="width: 123px; height: 47px; background: rgb(10, 20, 30)">Hello adopted</div>',
                    'text/html'
                  );
                  const svgDoc = new DOMParser().parseFromString(
                    '<svg xmlns="http://www.w3.org/2000/svg" id="imported-svg" width="88" height="44" viewBox="0 0 88 44"><rect width="88" height="44" fill="green"/></svg>',
                    'image/svg+xml'
                  );
                  globalThis.__card = htmlDoc.querySelector("#adopted-card");
                  globalThis.__svg = svgDoc.querySelector("svg");
                  return [
                    document.querySelector("#adopted-card") === null,
                    document.querySelector("#imported-svg") === null,
                    __card.ownerDocument === htmlDoc,
                    __svg.ownerDocument === svgDoc,
                  ];
                })()"##
            )
            .unwrap(),
        "[true,true,true,true]"
    );

    // Adopt the HTML node and import the SVG node into the live document, then commit a frame.
    assert_eq!(
        runner
            .eval(
                r##"(() => {
                  const adopted = document.adoptNode(globalThis.__card);
                  document.body.appendChild(adopted);
                  const imported = document.importNode(globalThis.__svg, true);
                  document.body.appendChild(imported);
                  return [
                    adopted === globalThis.__card,
                    adopted.ownerDocument === document,
                    imported !== globalThis.__svg,
                    imported.ownerDocument === document,
                    imported.namespaceURI,
                    document.querySelector("#adopted-card") === adopted,
                    document.querySelector("#imported-svg") === imported,
                  ];
                })()"##
            )
            .unwrap(),
        r#"[true,true,true,true,"http://www.w3.org/2000/svg",true,true]"#
    );

    let frame_tick = tick(origin, 2, 32);
    let frame = runner.frame(frame_tick).unwrap();
    assert_eq!(frame.frame, FrameId(2));
    let mut compositor = Compositor::new(mailbox, Arc::new(HitSnapshots::default()));
    let resource_seq = updates
        .try_iter()
        .last()
        .map_or(squark_types::render::ResourceSeq(0), |update| update.seq);
    assert!(
        compositor
            .adopt_latest(frame_tick.viewport, resource_seq)
            .is_some()
    );

    let card_rect = runner.query_rect("#adopted-card").unwrap().unwrap();
    assert_eq!(card_rect.size.width, 123.0);
    assert_eq!(card_rect.size.height, 47.0);

    let svg_rect = runner.query_rect("#imported-svg").unwrap().unwrap();
    assert_eq!(svg_rect.size.width, 88.0);
    assert_eq!(svg_rect.size.height, 44.0);
}

#[test]
fn domparser_does_not_journal_to_the_live_engine_until_adopted() {
    support::initialize_v8();
    let trace = Trace::default();
    let engine = Engine::new(trace.clone());
    let journal = engine.journal.clone();
    let origin = Instant::now();
    let packed = fixture(&[]);
    let mut runner = DocumentRunner::new(
        options(origin, true),
        &packed,
        sources(|_| None),
        Box::new(engine),
        Box::new(Paint(trace)),
        move || origin,
    )
    .unwrap();
    runner.frame(tick(origin, 1, 16)).unwrap();
    let baseline = journal.borrow().records.len();

    assert_eq!(
        runner
            .eval(
                r#"(() => {
                  const doc = new DOMParser().parseFromString(
                    '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 40 20"><rect width="40" height="20"/></svg>',
                    'image/svg+xml'
                  );
                  const svg = doc.documentElement;
                  svg.setAttribute("width", "40");
                  svg.setAttribute("height", "20");
                  globalThis.__inertSvg = svg;
                  return svg.outerHTML;
                })()"#
            )
            .unwrap(),
        r#""<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 40 20\" width=\"40\" height=\"20\"><rect width=\"40\" height=\"20\"/></svg>""#
    );
    runner.frame(tick(origin, 2, 32)).unwrap();
    assert_eq!(
        journal.borrow().records.len(),
        baseline,
        "DOMParser parsing and mutating inert nodes must not emit live journal records"
    );

    runner
        .eval("document.body.appendChild(document.adoptNode(globalThis.__inertSvg))")
        .unwrap();
    runner.frame(tick(origin, 3, 48)).unwrap();
    assert!(
        journal.borrow().records.len() > baseline,
        "adopting and appending a parsed node must journal it into the live document"
    );
}

#[test]
fn domparser_matches_pinned_chromium() {
    let probe = r##"(() => {
      const dp = new DOMParser();
      const svgDoc = dp.parseFromString(
        '<?xml version="1.0" encoding="UTF-8"?><svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" viewBox="0 0 100 50"><rect width="10" height="20"/><use xlink:href="#a"/></svg>',
        'image/svg+xml'
      );
      const svg = svgDoc.querySelector('svg');
      const use = svgDoc.querySelector('use');
      const badSvg = dp.parseFromString('<svg xmlns="http://www.w3.org/2000/svg"><rect></svg>', 'image/svg+xml');
      const htmlDoc = dp.parseFromString('<!DOCTYPE html><html><head><title>T</title></head><body><div id="a">Hi</div><noscript><p>N</p></noscript></body></html>', 'text/html');
      const quirksDoc = dp.parseFromString('<div>Q</div>', 'text/html');
      const xmlDoc = dp.parseFromString('<root><Item Attr="1">txt</Item></root>', 'application/xml');
      let badType = false;
      try { dp.parseFromString('<a/>', 'text/plain'); } catch (e) { badType = e instanceof TypeError; }
      return {
        svgNs: svg.namespaceURI,
        svgLocal: svg.localName,
        svgNodeName: svg.nodeName,
        svgXmlns: svg.getAttribute('xmlns'),
        svgViewBox: svg.getAttribute('viewBox'),
        svgViewBoxLower: svg.getAttribute('viewbox'),
        svgQueryUpper: svgDoc.querySelector('SVG') !== null,
        svgParserError: svgDoc.querySelector('parsererror') !== null,
        useXlink: use.getAttribute('xlink:href'),
        useXlinkNs: use.getAttributeNS('http://www.w3.org/1999/xlink', 'href'),
        badSvgHasError: badSvg.querySelector('parsererror') !== null,
        htmlCompat: htmlDoc.compatMode,
        quirksCompat: quirksDoc.compatMode,
        htmlTitle: htmlDoc.title,
        htmlDivNodeName: htmlDoc.querySelector('#a').nodeName,
        htmlNoscriptChild: htmlDoc.querySelector('noscript > p').tagName,
        htmlDefaultViewNull: htmlDoc.defaultView === null,
        xmlItemLocal: xmlDoc.querySelector('Item').localName,
        xmlItemLowerMiss: xmlDoc.querySelector('item') === null,
        xmlAttrCase: [xmlDoc.querySelector('Item').getAttribute('Attr'), xmlDoc.querySelector('Item').getAttribute('attr')],
        badType,
        execCommandUnknown: document.execCommand('nonexistentCommand'),
        querySupportedUnknown: document.queryCommandSupported('nonexistentCommand'),
        queryEnabledUnknown: document.queryCommandEnabled('nonexistentCommand'),
      };
    })()"##;

    let pack = open_basic_pack();
    let (mut runner, _mailbox, _updates, origin) = real_engine_runner(&pack, "");
    runner.frame(tick(origin, 1, 16)).unwrap();
    let squark_json: serde_json::Value =
        serde_json::from_str(&runner.eval(probe).unwrap()).unwrap();

    let node_script = format!(
        r#"
        const {{ chromium }} = require("playwright-core");
        const {{ findChromiumPath }} = require("./scripts/chromium-path.mjs");
        (async () => {{
          const browser = await chromium.launch({{ executablePath: findChromiumPath(), headless: true, args: ["--no-sandbox"] }});
          const page = await browser.newPage();
          const result = await page.evaluate(() => ({probe}));
          console.log(JSON.stringify(result));
          await browser.close();
        }})().catch(err => {{ console.error(err); process.exit(1); }});
        "#
    );
    let output = crate::timeout::output(
        Command::new("node")
            .args(["-e", &node_script])
            .current_dir(key(".")),
        Duration::from_secs(60),
    )
    .expect("run pinned Chromium comparison");
    assert!(
        output.status.success(),
        "Chromium probe failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let chromium_json: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("parse Chromium JSON");
    assert_eq!(squark_json, chromium_json);
}
