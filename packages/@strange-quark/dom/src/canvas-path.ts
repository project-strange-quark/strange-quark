// SPDX-License-Identifier: MIT

import { Command } from "./canvas-stream.js";
import type { PathCommand } from "./canvas-stream.js";
import { DOMException } from "./events.js";

type Point = [number, number];
type Radius = number | { x?: number; y?: number };
type Radii = Radius | Iterable<Radius>;

const point = (x: number, y: number): Point => [x, y];
const move = (...coordinates: Point): PathCommand => ({ op: Command.Move, coordinates });
const line = (...coordinates: Point): PathCommand => ({ op: Command.Line, coordinates });
const close: PathCommand = { op: Command.Close, coordinates: [] };

export function finite(values: readonly unknown[]): number[] | null {
  const converted = values.map(Number);
  return converted.every(Number.isFinite) ? converted : null;
}

function radius(value: Radius): Point {
  if (value !== null && typeof value === "object") return [Number(value.x ?? 0), Number(value.y ?? 0)];
  const scalar = Number(value);
  return [scalar, scalar];
}

function roundedRect(x: number, y: number, w: number, h: number, input: Radii): PathCommand[] {
  const scalars = input != null && typeof input === "object" && Symbol.iterator in input
    ? Array.from(input, radius) : [radius(input as Radius)];
  if (scalars.length < 1 || scalars.length > 4) throw new RangeError("roundRect requires one to four radii");
  if (!scalars.flat().every(Number.isFinite)) return [];
  if (scalars.some(([rx, ry]) => rx < 0 || ry < 0)) throw new RangeError("roundRect radii must be non-negative");

  const [first, second = first, third = first, fourth = second] = scalars;
  const width = Math.abs(w), height = Math.abs(h);
  const top = first[0] + second[0], bottom = third[0] + fourth[0];
  const left = first[1] + fourth[1], right = second[1] + third[1];
  const ratio = (size: number, sum: number) => sum === 0 ? 1 : size / sum;
  const scale = Math.min(1, ratio(width, top), ratio(width, bottom), ratio(height, left), ratio(height, right));
  const corners = [first, second, third, fourth].map(([rx, ry]) => [rx * scale, ry * scale] as Point);
  const [tl, tr, br, bl] = corners;
  const sx = w < 0 ? -1 : 1, sy = h < 0 ? -1 : 1;
  const map = (px: number, py: number): Point => [x + sx * px, y + sy * py];
  const angle = (radians: number) => sx < 0 ? (sy < 0 ? Math.PI + radians : Math.PI - radians) : sy * radians;
  function corner(cx: number, cy: number, radii: Point, start: number, end: number): PathCommand {
    return {
      op: Command.Ellipse,
      coordinates: [...map(cx, cy), ...radii, 0, angle(start), angle(end)],
      anticlockwise: sx !== sy,
    };
  }
  return [
    move(...map(tl[0], 0)), line(...map(width - tr[0], 0)),
    corner(width - tr[0], tr[1], tr, -Math.PI / 2, 0),
    line(...map(width, height - br[1])), corner(width - br[0], height - br[1], br, 0, Math.PI / 2),
    line(...map(bl[0], height)), corner(bl[0], height - bl[1], bl, Math.PI / 2, Math.PI),
    line(...map(0, tl[1])), corner(tl[0], tl[1], tl, Math.PI, 3 * Math.PI / 2), close,
    move(x, y),
  ];
}

export class CanvasPath {
  #append: (commands: readonly PathCommand[]) => void;

  protected constructor(append: (commands: readonly PathCommand[]) => void) { this.#append = append; }

  #coordinates(op: PathCommand["op"], values: number[]): void {
    const coordinates = finite(values);
    if (coordinates !== null) this.#append([{ op, coordinates }]);
  }

  closePath(): void { this.#append([close]); }
  moveTo(x: number, y: number): void { this.#coordinates(Command.Move, [x, y]); }
  lineTo(x: number, y: number): void { this.#coordinates(Command.Line, [x, y]); }
  quadraticCurveTo(cpx: number, cpy: number, x: number, y: number): void {
    this.#coordinates(Command.Quadratic, [cpx, cpy, x, y]);
  }
  bezierCurveTo(cp1x: number, cp1y: number, cp2x: number, cp2y: number, x: number, y: number): void {
    this.#coordinates(Command.Cubic, [cp1x, cp1y, cp2x, cp2y, x, y]);
  }
  rect(x: number, y: number, width: number, height: number): void {
    const values = finite([x, y, width, height]);
    if (values === null) return;
    const [left, top, w, h] = values;
    const right = left + w, bottom = top + h;
    if (!Number.isFinite(right) || !Number.isFinite(bottom)) return;
    this.#append([move(left, top), line(right, top), line(right, bottom), line(left, bottom), close, move(left, top)]);
  }
  roundRect(x: number, y: number, width: number, height: number, radii: Radii = 0): void {
    const values = finite([x, y, width, height]);
    if (values === null) return;
    const [left, top, w, h] = values;
    const commands = roundedRect(left, top, w, h, radii);
    if (commands.length !== 0 && commands.every(command => command.coordinates.every(Number.isFinite))) this.#append(commands);
  }
  arc(x: number, y: number, radius: number, start: number, end: number, anticlockwise = false): void {
    this.ellipse(x, y, radius, radius, 0, start, end, anticlockwise);
  }
  ellipse(x: number, y: number, rx: number, ry: number, rotation: number, start: number, end: number, anticlockwise = false): void {
    const coordinates = finite([x, y, rx, ry, rotation, start, end]);
    if (coordinates === null) return;
    if (coordinates[2] < 0 || coordinates[3] < 0) throw new DOMException("Ellipse radii must be non-negative", "IndexSizeError");
    this.#append([{ op: Command.Ellipse, coordinates, anticlockwise: Boolean(anticlockwise) }]);
  }
}

const stored = new WeakMap<Path2D, PathCommand[]>();

export class Path2D extends CanvasPath {
  constructor(path?: Path2D | string) {
    const commands = path === undefined ? [] :
      path instanceof Path2D ? [...pathCommands(path)] : parsePath(String(path));
    super(additional => { for (const command of additional) commands.push(command); });
    stored.set(this, commands);
  }
}

export function pathCommands(path: Path2D): readonly PathCommand[] {
  return stored.get(path)!;
}

// SVG2 endpoint-to-center conversion. Radii are expanded if the endpoints
// would otherwise be outside the ellipse.
function svgEllipse(start: Point, end: Point, rx: number, ry: number, degrees: number, large: number, sweep: number): PathCommand | undefined {
  if (start[0] === end[0] && start[1] === end[1]) return undefined;
  if (rx === 0 || ry === 0) return line(...end);
  rx = Math.abs(rx);
  ry = Math.abs(ry);
  const rotation = (degrees % 360) * Math.PI / 180;
  const cos = Math.cos(rotation), sin = Math.sin(rotation);
  const halfX = start[0] / 2 - end[0] / 2, halfY = start[1] / 2 - end[1] / 2;
  const x = cos * halfX + sin * halfY, y = -sin * halfX + cos * halfY;
  const scale = Math.hypot(x / rx, y / ry);
  if (scale > 1) {
    rx *= scale;
    ry *= scale;
  }
  const u = x / rx, v = y / ry;
  const length = Math.hypot(u, v);
  const magnitude = Math.sqrt(Math.max(0, 1 - length * length));
  const direction = large === sweep ? -1 : 1;
  const cx = direction * (v / length) * magnitude;
  const cy = -direction * (u / length) * magnitude;
  const centerX = cos * cx * rx - sin * cy * ry + start[0] / 2 + end[0] / 2;
  const centerY = sin * cx * rx + cos * cy * ry + start[1] / 2 + end[1] / 2;
  const startAngle = Math.atan2(v - cy, u - cx);
  const endAngle = Math.atan2(-v - cy, -u - cx);
  const coordinates = [centerX, centerY, rx, ry, rotation, startAngle, endAngle];
  if (!coordinates.every(Number.isFinite)) return line(...end);
  return { op: Command.Ellipse, coordinates, anticlockwise: sweep === 0 };
}

const arity: Readonly<Record<string, number>> = { M: 2, L: 2, H: 1, V: 1, C: 6, S: 4, Q: 4, T: 2, A: 7, Z: 0 };
const svgNumber = /[+-]?(?:[0-9]+(?:\.[0-9]*)?|\.[0-9]+)(?:[eE][+-]?[0-9]+)?/y;
const svgWhitespace = /[\t\n\r ]*/y;

function parsePath(data: string): PathCommand[] {
  const commands: PathCommand[] = [];
  let index = 0;
  let current = point(0, 0), start = current;
  let control = current, previous = "", letter = "";
  const space = () => {
    svgWhitespace.lastIndex = index;
    svgWhitespace.exec(data);
    index = svgWhitespace.lastIndex;
  };
  function parameters(kind: string, explicit: boolean): number[] | null {
    const values: number[] = [];
    for (let i = 0; i < arity[kind]; i++) {
      space();
      if ((i !== 0 || !explicit) && data[index] === ",") {
        index++;
        space();
      }
      if (kind === "A" && (i === 3 || i === 4)) {
        const bit = data[index++];
        if (bit !== "0" && bit !== "1") return null;
        values.push(Number(bit));
      } else {
        svgNumber.lastIndex = index;
        const token = svgNumber.exec(data);
        if (token === null || !Number.isFinite(Number(token[0]))) return null;
        index = svgNumber.lastIndex;
        values.push(Number(token[0]));
      }
    }
    return values;
  }
  while (index < data.length) {
    space();
    const explicit = /[a-z]/i.test(data[index] ?? "");
    if (explicit) letter = data[index++];
    const kind = letter.toUpperCase();
    if (!(kind in arity) || (commands.length === 0 && kind !== "M")) break;
    if (kind === "Z") {
      commands.push(close);
      current = start;
      previous = kind;
      letter = "";
      continue;
    }
    const args = parameters(kind, explicit);
    if (args === null) break; // SVG retains any complete segments before the error.
    const relative = letter !== kind;
    const at = (i: number): Point => relative ? [current[0] + args[i], current[1] + args[i + 1]] : [args[i], args[i + 1]];
    const reflect = (): Point => [2 * current[0] - control[0], 2 * current[1] - control[1]];
    let end = current;
    let segment: PathCommand | undefined;
    switch (kind) {
      case "M":
        start = end = at(0);
        segment = move(...end);
        letter = relative ? "l" : "L";
        break;
      case "L":
        end = at(0);
        segment = line(...end);
        break;
      case "H":
        end = [args[0] + (relative ? current[0] : 0), current[1]];
        segment = line(...end);
        break;
      case "V":
        end = [current[0], args[0] + (relative ? current[1] : 0)];
        segment = line(...end);
        break;
      case "C":
      case "S": {
        const smooth = kind === "S";
        const first = smooth ? (previous === "C" || previous === "S" ? reflect() : current) : at(0);
        control = at(smooth ? 0 : 2);
        end = at(smooth ? 2 : 4);
        segment = { op: Command.Cubic, coordinates: [...first, ...control, ...end] };
        break;
      }
      case "Q":
      case "T":
        control = kind === "Q" ? at(0) : previous === "Q" || previous === "T" ? reflect() : current;
        end = at(kind === "Q" ? 2 : 0);
        segment = { op: Command.Quadratic, coordinates: [...control, ...end] };
        break;
      case "A":
        end = at(5);
        segment = svgEllipse(current, end, args[0], args[1], args[2], args[3], args[4]);
        break;
    }
    if (!end.every(Number.isFinite) || segment?.coordinates.some(value => !Number.isFinite(value))) break;
    if (segment !== undefined) commands.push(segment);
    current = end;
    previous = kind;
  }
  return commands;
}
