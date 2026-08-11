// Real line-level diffing (Myers O(ND)) — replaces the naive "mark every
// shifted line" comparison that made small edits look like full rewrites.

export type DiffLine =
  | { type: "equal"; oldNo: number; newNo: number; text: string }
  | { type: "delete"; oldNo: number; newNo: null; text: string }
  | { type: "insert"; oldNo: null; newNo: number; text: string };

export interface DiffResult {
  lines: DiffLine[];
  additions: number;
  deletions: number;
}

function splitLines(text: string): string[] {
  if (text === "") return [];
  const lines = text.split("\n");
  if (lines[lines.length - 1] === "") lines.pop(); // trailing newline
  return lines;
}

type Edit = { type: "equal" | "delete" | "insert"; text: string };

function myersEditScript(a: string[], b: string[]): Edit[] {
  const n = a.length;
  const m = b.length;
  const max = n + m;
  const offset = max;
  // v[k] = furthest x on diagonal k; k = x - y.
  const v = new Array<number>(2 * max + 1).fill(0);
  const trace: number[][] = [];
  let distance = -1;

  outer: for (let d = 0; d <= max; d++) {
    trace.push(v.slice());
    for (let k = -d; k <= d; k += 2) {
      let x: number;
      if (k === -d || (k !== d && v[offset + k - 1] < v[offset + k + 1])) {
        x = v[offset + k + 1];
      } else {
        x = v[offset + k - 1] + 1;
      }
      let y = x - k;
      while (x < n && y < m && a[x] === b[y]) {
        x++;
        y++;
      }
      v[offset + k] = x;
      if (x >= n && y >= m) {
        distance = d;
        break outer;
      }
    }
  }

  if (distance < 0) {
    // Shouldn't happen (max is an upper bound), but never return garbage.
    return [];
  }

  // Backtrack the trace to build the edit script (reversed, then flipped).
  const edits: Edit[] = [];
  let x = n;
  let y = m;
  for (let d = distance; d > 0; d--) {
    const prevV = trace[d];
    const k = x - y;
    let prevK: number;
    if (k === -d || (k !== d && prevV[offset + k - 1] < prevV[offset + k + 1])) {
      prevK = k + 1;
    } else {
      prevK = k - 1;
    }
    const prevX = prevV[offset + prevK];
    const prevY = prevX - prevK;
    while (x > prevX && y > prevY) {
      edits.push({ type: "equal", text: a[x - 1] });
      x--;
      y--;
    }
    if (x === prevX) {
      edits.push({ type: "insert", text: b[y - 1] });
      y--;
    } else {
      edits.push({ type: "delete", text: a[x - 1] });
      x--;
    }
  }
  while (x > 0 && y > 0) {
    edits.push({ type: "equal", text: a[x - 1] });
    x--;
    y--;
  }
  while (x > 0) {
    edits.push({ type: "delete", text: a[x - 1] });
    x--;
  }
  while (y > 0) {
    edits.push({ type: "insert", text: b[y - 1] });
    y--;
  }
  edits.reverse();
  return edits;
}

export function diffLines(oldText: string, newText: string): DiffResult {
  const a = splitLines(oldText);
  const b = splitLines(newText);
  const edits = myersEditScript(a, b);

  const lines: DiffLine[] = [];
  let oldNo = 0;
  let newNo = 0;
  let additions = 0;
  let deletions = 0;

  for (const e of edits) {
    if (e.type === "equal") {
      oldNo++;
      newNo++;
      lines.push({ type: "equal", oldNo, newNo, text: e.text });
    } else if (e.type === "delete") {
      oldNo++;
      deletions++;
      lines.push({ type: "delete", oldNo, newNo: null, text: e.text });
    } else {
      newNo++;
      additions++;
      lines.push({ type: "insert", oldNo: null, newNo, text: e.text });
    }
  }
  return { lines, additions, deletions };
}

// Aligned rows for a side-by-side view: consecutive deletes+inserts pair up
// into (old, new) row pairs so both columns stay height-synced.
export interface SideBySideRow {
  old?: DiffLine;
  new?: DiffLine;
}

export function toSideBySideRows(lines: DiffLine[]): SideBySideRow[] {
  const rows: SideBySideRow[] = [];
  let i = 0;
  while (i < lines.length) {
    const line = lines[i];
    if (line.type === "equal") {
      rows.push({ old: line, new: line });
      i++;
      continue;
    }
    const olds: DiffLine[] = [];
    const news: DiffLine[] = [];
    while (i < lines.length && lines[i].type === "delete") olds.push(lines[i++]);
    while (i < lines.length && lines[i].type === "insert") news.push(lines[i++]);
    const count = Math.max(olds.length, news.length);
    for (let j = 0; j < count; j++) {
      rows.push({ old: olds[j], new: news[j] });
    }
  }
  return rows;
}
