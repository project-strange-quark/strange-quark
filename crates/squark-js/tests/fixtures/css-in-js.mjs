// SPDX-License-Identifier: MIT

import createCache from "@emotion/cache";
import { CacheProvider, Global, css, jsx } from "@emotion/react";
import { createElement as h } from "react";
import { flushSync } from "react-dom";
import { createRoot } from "react-dom/client";
import styled, { StyleSheetManager, keyframes } from "styled-components";

const cache = createCache({ key: "squark", container: document.head, speedy: true });
const pulse = keyframes`from { opacity: 0.5; } to { opacity: 1; }`;
const Button = styled.button`
  color: ${props => props.$tone};
  animation: ${pulse} 120ms ease;
`;
const root = createRoot(document.body);

globalThis.renderCss = (tone, textMode = false) => {
  flushSync(() => root.render(h("main", null,
    h(CacheProvider, { value: cache },
      h(Global, { styles: css`body { background: lavender; }` }),
      jsx("div", { id: "emotion", css: css`color: ${tone}; &:hover { opacity: 0.7; }` }, tone),
    ),
    h(StyleSheetManager, { disableCSSOMInjection: textMode },
      h(Button, { id: "styled", $tone: tone }, tone)),
  )));
};

globalThis.unmountCss = () => {
  flushSync(() => root.unmount());
  cache.sheet.flush();
};
