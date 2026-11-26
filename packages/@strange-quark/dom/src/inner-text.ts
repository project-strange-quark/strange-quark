// SPDX-License-Identifier: MIT

import { Element, Text } from "./document.js";
import type { HTMLElement } from "./document.js";

type ReadStyle = (element: Element, property: string) => string;
type TextStyle = { visibility: string; whitespace: string; transform: string };

const blockTags = new Set("address article aside blockquote dd details dialog div dl dt fieldset figcaption figure footer form h1 h2 h3 h4 h5 h6 header hgroup hr li main menu nav ol optgroup option p pre section summary ul".split(" "));
const hiddenTags = new Set(["head", "script", "style", "template", "noscript"]);
const replaced = new Set(["audio", "canvas", "iframe", "img", "input", "textarea", "video"]);
const tableDisplay = new Map([
  ["table", "table"], ["caption", "table-caption"], ["tr", "table-row"], ["td", "table-cell"], ["th", "table-cell"],
]);
const blockDisplay = new Set(["block", "block flow", "flow-root", "flex", "grid", "list-item", "table", "table-caption"]);

export function renderedInnerText(root: HTMLElement, read: ReadStyle): string {
  const display = (element: Element): string => read(element, "display") ||
    (element.hasAttribute("hidden") || hiddenTags.has(element.localName) ? "none" :
      tableDisplay.get(element.localName) ?? (blockTags.has(element.localName) ? "block" : "inline"));

  if (!root.isConnected) return root.textContent ?? "";
  for (let element: Element | null = root; element !== null; element = element.parentElement) {
    if (display(element) === "none") return root.textContent ?? "";
  }

  let output = "";
  let requiredBreaks = 0;
  let space = false;
  let lineStart = true;
  let previousWord = false;

  function emit(text: string): void {
    if (requiredBreaks !== 0) {
      if (output.length !== 0) output += "\n".repeat(requiredBreaks);
      requiredBreaks = 0;
      lineStart = true;
    }
    if (space && !lineStart) output += " ";
    output += text;
    space = false;
    lineStart = text.endsWith("\n");
    previousWord = /[\p{L}\p{N}]$/u.test(text);
  }

  function hardBreak(text: string): void {
    space = false;
    emit(text);
  }

  function softBreak(count: number): void {
    requiredBreaks = Math.max(requiredBreaks, count);
    space = false;
    previousWord = false;
  }

  function appendText(text: string, style: TextStyle): void {
    if (style.visibility !== "visible" || text === "") return;
    let value = text.replace(/\r\n?/g, "\n");
    switch (style.transform) {
      case "uppercase": value = value.toUpperCase(); break;
      case "lowercase": value = value.toLowerCase(); break;
      case "capitalize":
        value = value.replace(/(^|[^\p{L}\p{N}])(\p{L})/gu, (match, prefix: string, letter: string, index: number) =>
          index === 0 && prefix === "" && previousWord && !space ? match : prefix + letter.toUpperCase());
        break;
    }
    const mode = style.whitespace.split(" ")[0];
    if (["pre", "pre-wrap", "break-spaces", "preserve"].includes(mode)) {
      emit(value);
      return;
    }
    const keepNewlines = mode === "pre-line" || mode === "preserve-breaks";
    for (const part of value.match(/[\t\f ]+|\n|[^\t\f \n]+/g) ?? []) {
      if (part === "\n" && keepNewlines) hardBreak(part);
      else if (/^[\t\f \n]/.test(part)) space = true;
      else emit(part);
    }
  }

  function styleOf(element: Element, parent: TextStyle): TextStyle {
    return {
      visibility: read(element, "visibility") || parent.visibility,
      whitespace: read(element, "white-space") || read(element, "white-space-collapse") ||
        (element.localName === "pre" ? "pre" : parent.whitespace),
      transform: read(element, "text-transform") || parent.transform,
    };
  }

  function collect(parent: Element, style: TextStyle): void {
    if (replaced.has(parent.localName)) return;
    for (const child of parent.childNodes) {
      if (child instanceof Text) {
        appendText(child.data, style);
      } else if (child instanceof Element) {
        const box = display(child);
        if (box === "none") continue;
        const own = styleOf(child, style);
        const visible = own.visibility === "visible" && box !== "contents";
        const boundaries = !visible ? 0 : child.localName === "p" ? 2 : blockDisplay.has(box) ? 1 : 0;
        if (boundaries !== 0) softBreak(boundaries);
        collect(child, own);
        if (visible) {
          if (child.localName === "br") hardBreak("\n");
          if (box === "table-cell" && following(child, "table-cell")) hardBreak("\t");
          if (box === "table-row" && followingRow(child)) hardBreak("\n");
        }
        if (boundaries !== 0) softBreak(boundaries);
      }
    }
  }

  function following(element: Element, box: string): boolean {
    for (let sibling = element.nextElementSibling; sibling !== null; sibling = sibling.nextElementSibling) {
      if (display(sibling) === box) return true;
    }
    return false;
  }

  function followingRow(row: Element): boolean {
    if (following(row, "table-row")) return true;
    const group = row.parentElement;
    if (group?.parentElement === null || group === null || display(group.parentElement) !== "table") return false;
    for (let sibling = group.nextElementSibling; sibling !== null; sibling = sibling.nextElementSibling) {
      if (display(sibling) !== "none" && Array.from(sibling.children).some(child => display(child) === "table-row")) return true;
    }
    return false;
  }

  const inherited = { visibility: "visible", whitespace: "normal", transform: "none" };
  const ancestors: Element[] = [];
  for (let element: Element | null = root; element !== null; element = element.parentElement) ancestors.push(element);
  collect(root, ancestors.reduceRight((style, element) => styleOf(element, style), inherited));
  return output;
}
