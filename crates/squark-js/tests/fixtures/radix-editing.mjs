// SPDX-License-Identifier: MIT

import { FocusScope } from "@radix-ui/react-focus-scope";
import { DismissableLayer } from "@radix-ui/react-dismissable-layer";
import { createElement as h, useState } from "react";
import { createPortal, flushSync } from "react-dom";
import { createRoot } from "react-dom/client";

const mount = document.body.appendChild(document.createElement("div"));
const portal = document.body.appendChild(document.createElement("div"));
portal.id = "portal";
globalThis.radixEvents = [];

function App() {
  const [open, setOpen] = useState(false);
  const [first, setFirst] = useState(true);
  const [text, setText] = useState("modal");
  globalThis.openRadix = () => flushSync(() => { setFirst(true); setOpen(true); });
  globalThis.removeFocusedField = () => flushSync(() => setFirst(false));

  return h("main", null,
    h("button", { id: "trigger" }, "Trigger"),
    h("button", { id: "outside" }, "Outside"),
    open && createPortal(h(FocusScope, { id: "scope", trapped: true, loop: true },
      h(DismissableLayer, {
        id: "layer",
        onEscapeKeyDown: e => radixEvents.push(["escape", e.target.id]),
        onFocusOutside: e => e.preventDefault(),
        onPointerDownOutside: e => radixEvents.push(["outside", e.target.id]),
        onDismiss() { radixEvents.push(["dismiss"]); setOpen(false); },
      },
      first && h("input", {
        id: "first", value: text, onChange: e => setText(e.target.value),
        onCompositionStart: e => radixEvents.push(["compositionstart", e.data]),
        onCompositionUpdate: e => radixEvents.push(["compositionupdate", e.data]),
        onCompositionEnd: e => radixEvents.push(["compositionend", e.data]),
      }),
      h("div", { style: { display: "none" } }, h("button", { id: "hidden" }, "Hidden")),
      h("button", { id: "disabled", disabled: true }, "Disabled"),
      h("button", { id: "last" }, "Last"),
      h("output", { id: "echo" }, text))),
    portal),
  );
}

const root = createRoot(mount);
flushSync(() => root.render(h(App)));
