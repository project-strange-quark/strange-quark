// SPDX-License-Identifier: MIT

import { createElement as h, useState } from "react";
import { flushSync } from "react-dom";
import { createRoot } from "react-dom/client";

function App() {
  const [count, setCount] = useState(0);
  const [text, setText] = useState("");

  return h("section", null,
    h("button", { id: "counter", onClick: () => setCount(count + 1) },
      h("span", { id: "count" }, String(count))),
    h("input", {
      id: "editor", value: text,
      onChange: event => setText(event.target.value),
    }),
    h("output", { id: "echo" }, text),
    h("form", { id: "actions", action: () => {} },
      h("input", { name: "title", defaultValue: "note" }),
      h("button", { id: "publish", name: "mode", value: "publish",
        formAction: data => { globalThis.formActionData = [...data]; } }, "Publish")),

  );
}

const root = createRoot(document.body);
flushSync(() => root.render(h(App)));
