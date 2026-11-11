// TypeScript stripping as the browser kernel does it: sucrase (TSX, then
// TS) with a source map, then acorn over the result. Needs
// dist/strip-bundle.js first, which defines globalThis.benchStrip.
const cell = (i) => `
interface Point${i}<T extends number = number> { x: T; y: T; label?: string }
type Shape${i} = { kind: "circle"; r: number } | { kind: "rect"; w: number; h: number };
enum Color${i} { Red = "red", Green = "green" }
export function area${i}(s: Shape${i}): number {
  switch (s.kind) {
    case "circle": return Math.PI * s.r ** 2;
    case "rect": return s.w * s.h;
  }
}
const points${i}: Array<Point${i}> = [{ x: 1, y: 2 }, { x: 3, y: 4, label: "b" }];
class Grid${i}<T> {
  private cells: Map<string, T> = new Map();
  constructor(public readonly width: number, public readonly height: number) {}
  set(x: number, y: number, value: T): this { this.cells.set(x + ":" + y, value); return this; }
  get(x: number, y: number): T | undefined { return this.cells.get(x + ":" + y); }
}
const total${i} = points${i}.reduce((acc: number, p: Point${i}) => acc + p.x * p.y, 0) as number;
const el${i} = <div className="cell">{total${i}}</div>;
`;
const source = Array.from({ length: 60 }, (_, i) => cell(i)).join("\n");
globalThis.workload = {
  run() {
    const out = globalThis.benchStrip(source);
    return String(out.length) + " bytes from " + String(source.length);
  },
};
