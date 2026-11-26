// SPDX-License-Identifier: MIT

use super::*;

#[test]
fn packed_location_drives_hash_router_and_session_history() {
    support::initialize_v8();
    let origin = Instant::now();
    let mut options = options(origin, true);
    options.url = "https://squark.invalid:8443/app/index.html?route=initial#todos".into();
    let trace = Trace::default();
    let mut runner = DocumentRunner::new(
        options,
        &fixture(&[(ScriptKind::InlineClassic, r#"
            globalThis.startedWith = [window.location.hash, location === document.location];
            globalThis.events = [];
            onhashchange = event => events.push([event instanceof HashChangeEvent, event.oldURL, event.newURL]);
            onpopstate = event => events.push([event instanceof PopStateEvent, event.state]);
        "#, "/app/index.html")]),
        sources(|_| None),
        Box::new(Engine::new(trace.clone())),
        Box::new(Paint(trace)),
        move || origin,
    ).unwrap();
    assert_eq!(runner.eval("startedWith").unwrap(), r##"["#todos",true]"##);
    assert_eq!(
        runner
            .eval(
                r##"
        [location.href, location.origin, location.protocol, location.host, location.hostname,
         location.port, location.pathname, location.search, String(location),
         new URL("/fonts/app.woff2", location.origin).href]
    "##
            )
            .unwrap(),
        r##"["https://squark.invalid:8443/app/index.html?route=initial#todos","https://squark.invalid:8443","https:","squark.invalid:8443","squark.invalid","8443","/app/index.html","?route=initial","https://squark.invalid:8443/app/index.html?route=initial#todos","https://squark.invalid:8443/fonts/app.woff2"]"##
    );

    assert_eq!(
        runner
            .eval(
                r##"
        location.hash = "active";
        [location.hash, history.length, events.length, document.URL === location.href]
    "##
            )
            .unwrap(),
        r##"["#active",2,1,true]"##
    );
    runner.run_ready().unwrap();
    assert_eq!(
        runner.eval("events.splice(0)").unwrap(),
        r##"[[true,null],[true,"https://squark.invalid:8443/app/index.html?route=initial#todos","https://squark.invalid:8443/app/index.html?route=initial#active"]]"##
    );

    assert_eq!(
        runner
            .eval(
                r##"
        const data = { enabled: true };
        history.pushState(data, "", "?route=next#a");
        data.enabled = false;
        history.replaceState({ count: 2 }, "", "#b");
        location.replace("#c");
        history.replaceState({ count: 3 }, "");
        location.assign("#d");
        history.back();
        [history.length, history.state, location.search, location.hash, events.length]
    "##
            )
            .unwrap(),
        r##"[4,null,"?route=next","#d",2]"##
    );
    runner.run_ready().unwrap();
    assert_eq!(
        runner
            .eval(
                r##"
        [history.state, location.hash, events.map(entry => entry.length), events[4]]
    "##
            )
            .unwrap(),
        r##"[{"count":3},"#c",[2,2,3,3,2,3],[true,{"count":3}]]"##
    );
    runner.eval("history.forward();").unwrap();
    runner.run_ready().unwrap();
    assert_eq!(
        runner.eval("[history.state, location.hash]").unwrap(),
        r##"[null,"#d"]"##
    );
    runner.eval("history.go(-2);").unwrap();
    runner.run_ready().unwrap();
    assert_eq!(
        runner.eval("[history.state, location.hash]").unwrap(),
        r##"[null,"#active"]"##
    );
    assert_eq!(runner.eval(r##"
        let error;
        try { history.pushState({}, "", "https://outside.test") } catch (exception) { error = exception.name; }
        let failedClone;
        try { history.pushState(() => {}, ""); } catch (exception) { failedClone = exception.name; }
        location.reload();
        [error, failedClone, history.length, location.hash]
    "##).unwrap(), r##"["SecurityError","DataCloneError",4,"#active"]"##);
    assert_eq!(
        runner
            .eval(
                r##"
        const data2 = { enabled: true };
        history.pushState(data2, "", "#tail");
        data2.enabled = false;
        [history.state.enabled, history.length, location.hash]
    "##
            )
            .unwrap(),
        r##"[true,3,"#tail"]"##
    );
}

#[test]
fn url_components_and_search_params_share_the_same_live_url() {
    support::initialize_v8();
    let no = Instant::now();
    let trace = Trace::default();
    let mut runner = DocumentRunner::new(
        options(no, true),
        &fixture(&[]),
        sources(|_| None),
        Box::new(Engine::new(trace.clone())),
        Box::new(Paint(trace)),
        move || no,
    )
    .unwrap();
    assert_eq!(
        runner
            .eval(
                r##"
        const url = new URL("http://example.org:82/old?a=a%20b&b=1#old");
        const params = url.searchParams;
        url.pathname = "/new path?question#hash";
        const path = url.pathname;
        url.protocol = "https:";
        url.username = "user@";
        url.password = "pass#";
        url.hostname = "ExAmPle.NET";
        url.port = "443";
        const keys = [url.origin,url.host,url.username,url.password,url.port];
        url.host = "[::1]:8443";
        url.hash = "#x y";
        url.search = "?replace=1&z=a%20b";
        const newSearch = params.get("z");
        params.set("replace", "two words");
        params.append("replace", "~");
        const modified = url.href;
        url.href = "https://example.test/replaced?again=3#";
        const same = params === url.searchParams;
        const previous = [params.get("again"), url.hash, url.href.endsWith("#")];
        url.search = "?";
        const empty = [params.size, url.search, url.href.endsWith("?#")];
        let badHref;
        try { url.href = "not absolute"; } catch(e) { badHref = e.name; }
        const frozen = new URL("data:text/plain,hi");
        frozen.pathname = "/ignored";
        [path, keys, newSearch, modified, same, previous, empty, badHref, frozen.pathname]
    "##
            )
            .unwrap(),
        r##"["/new%20path%3Fquestion%23hash",["https://example.net","example.net","user%40","pass%23",""],"a b","https://user%40:pass%23@[::1]:8443/new%20path%3Fquestion%23hash?replace=two+words&z=a+b&replace=%7E#x%20y",true,["3","",true],[0,"",true],"TypeError","text/plain,hi"]"##
    );
}
