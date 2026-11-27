// SPDX-License-Identifier: MIT

use super::domparser_tests::{open_basic_pack, real_engine_runner};
use super::*;

#[test]
fn idle_callbacks_run_once_the_document_task_turn_has_nothing_ready() {
    let pack = open_basic_pack();
    let (mut runner, _mailbox, _updates, origin) = real_engine_runner(&pack, "");
    runner.frame(tick(origin, 1, 16)).unwrap();
    runner
        .eval(
            r#"globalThis.idle = [];
            requestIdleCallback(deadline => {
              idle.push(["idle", deadline.didTimeout, deadline.timeRemaining() <= 50]);
              requestIdleCallback(() => idle.push(["next period"]));
            });
            setTimeout(() => idle.push(["timer"]), 0);"#,
        )
        .unwrap();
    runner.run_ready().unwrap();
    assert_eq!(
        runner.eval("idle").unwrap(),
        r#"[["timer"],["idle",false,true]]"#,
        "the ready timer runs first and the idle period follows it"
    );
}
