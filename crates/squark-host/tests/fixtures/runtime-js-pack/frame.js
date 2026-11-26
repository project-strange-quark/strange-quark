// SPDX-License-Identifier: MIT

requestAnimationFrame(() => {
  if (innerWidth === 40 && innerHeight === 30 && devicePixelRatio === 1.5) {
    document.documentElement.style.setProperty("background-color", globalThis.startupColor);
  }
});

window.addEventListener("resize", () => {
  if (innerWidth === 60 && devicePixelRatio === 2) {
    document.documentElement.style.setProperty("background-color", "#246820");
  }
});

document.getElementById("control").addEventListener("click", event => {
  if (event.isTrusted && event.clientX === 6 && event.clientY === 4) {
    document.documentElement.style.setProperty("background-color", "#dd6622");
  }
});
