// SPDX-License-Identifier: MIT

use super::domparser_tests::{open_basic_pack, real_engine_runner};
use super::*;

/// The real DOMPurify against squark-dom with the engine's html5ever parser.
/// The expected outputs are what DOMPurify returns in the pinned Chromium.
#[test]
fn dompurify_sanitises_payloads_as_in_chromium_through_the_engine_parser() {
    let pack = open_basic_pack();
    let (mut runner, _mailbox, _updates, origin) = real_engine_runner(&pack, "");
    runner.frame(tick(origin, 1, 16)).unwrap();
    let purify = std::fs::read_to_string(key("node_modules/dompurify/dist/purify.js"))
        .expect("Run npm install to fetch the DOMPurify test dependency");
    runner.eval(&format!("{purify};0")).unwrap();
    let payloads = std::fs::read_to_string(key(
        "packages/@strange-quark/dom/test/fixtures/dompurify-payloads.json",
    ))
    .unwrap();
    assert_eq!(
        runner
            .eval(&format!(
                r#"(() => {{
                  const mismatches = {payloads}
                    .map(([input, expected]) => [input, DOMPurify.sanitize(input), expected])
                    .filter(([, actual, expected]) => actual !== expected);
                  return [DOMPurify.isSupported, mismatches];
                }})()"#
            ))
            .unwrap(),
        "[true,[]]"
    );
}

#[test]
fn inert_documents_parse_markup_and_iterate_through_the_engine() {
    let pack = open_basic_pack();
    let (mut runner, _mailbox, _updates, origin) = real_engine_runner(&pack, "");
    runner.frame(tick(origin, 1, 16)).unwrap();
    assert_eq!(
        runner
            .eval(
                r##"(() => {
                  const inert = document.implementation.createHTMLDocument("inert");
                  inert.body.innerHTML = "<p id=a>x<b>y</b></p><template><i>z</i></template>";
                  const names = [];
                  const iterator = inert.createNodeIterator(inert.body, NodeFilter.SHOW_ELEMENT);
                  for (let node = iterator.nextNode(); node !== null; node = iterator.nextNode()) names.push(node.localName);
                  const parsed = new DOMParser().parseFromString("<table><tr><td>c</td></tr></table>", "text/html");
                  parsed.querySelector("td").innerHTML = "<em>cell</em>";
                  document.body.append(document.importNode(inert.body.firstChild, true), document.adoptNode(parsed.querySelector("table")));
                  return [
                    inert.defaultView, inert.title, names, inert.body.lastChild.content.firstChild.localName,
                    document.querySelector("#a b").textContent, document.querySelector("td").innerHTML,
                  ];
                })()"##
            )
            .unwrap(),
        r#"[null,"inert",["body","p","b","template"],"i","y","<em>cell</em>"]"#
    );
}
