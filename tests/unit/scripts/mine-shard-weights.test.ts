// mine-shard-weights.test.ts — pure-function tests for the log parser
// and weight extraction. The full pipeline (gh run view → write JSON)
// is integration-tested by actually running it once during T4.

import { describe, expect, it } from "bun:test";
import {
  computeWeights,
  parseLog,
  serializeWeights,
} from "../../../scripts/mine-shard-weights.ts";

const SAMPLE = `test (1)\tUNKNOWN STEP\t2026-05-25T11:26:40.000000Z ##[group]tests/unit/alpha.test.ts:
test (1)\tUNKNOWN STEP\t2026-05-25T11:26:41.000000Z (pass) some test [1.00ms]
test (1)\tUNKNOWN STEP\t2026-05-25T11:26:43.500000Z ##[group]tests/unit/beta.test.ts:
test (1)\tUNKNOWN STEP\t2026-05-25T11:26:45.000000Z (pass) another test
test (1)\tUNKNOWN STEP\t2026-05-25T11:26:48.000000Z ##[group]tests/unit/gamma.test.ts:
test (1)\tUNKNOWN STEP\t2026-05-25T11:26:50.000000Z ##[group]tests/unit/delta.test.ts:
test (2)\tUNKNOWN STEP\t2026-05-25T11:26:42.000000Z ##[group]tests/unit/epsilon.test.ts:
test (2)\tUNKNOWN STEP\t2026-05-25T11:26:55.000000Z ##[group]tests/unit/zeta.test.ts:
`;

describe("parseLog", () => {
  it("extracts file-start events with timestamps and jobs", () => {
    const events = parseLog(SAMPLE);
    expect(events.length).toBe(6);
    expect(events[0]).toMatchObject({
      job: "test (1)",
      file: "tests/unit/alpha.test.ts",
    });
    expect(events[5]).toMatchObject({
      job: "test (2)",
      file: "tests/unit/zeta.test.ts",
    });
  });

  it("preserves stream order", () => {
    const events = parseLog(SAMPLE);
    const files = events.map((e) => e.file);
    expect(files).toEqual([
      "tests/unit/alpha.test.ts",
      "tests/unit/beta.test.ts",
      "tests/unit/gamma.test.ts",
      "tests/unit/delta.test.ts",
      "tests/unit/epsilon.test.ts",
      "tests/unit/zeta.test.ts",
    ]);
  });

  it("ignores non-group lines", () => {
    // The sample has (pass) lines mixed in. They should not appear in the
    // event list; parseLog only emits ##[group]tests/unit/X.test.ts: matches.
    const events = parseLog(SAMPLE);
    expect(events.every((e) => e.file.startsWith("tests/unit/"))).toBe(true);
    expect(events.every((e) => e.file.endsWith(".test.ts"))).toBe(true);
  });

  it("handles empty input", () => {
    expect(parseLog("")).toEqual([]);
  });

  it("ignores malformed timestamps", () => {
    const bad = "test (1)\tUNKNOWN\tBAD-TS ##[group]tests/unit/x.test.ts:\n";
    expect(parseLog(bad)).toEqual([]);
  });

  it("rejects non-test-file groups (e.g. setup-bun action groups)", () => {
    const noise =
      "test (1)\tUNKNOWN\t2026-05-25T11:26:40.000Z ##[group]Run actions/checkout\n";
    expect(parseLog(noise)).toEqual([]);
  });
});

describe("computeWeights", () => {
  it("computes delta between consecutive file headers within a job", () => {
    const events = parseLog(SAMPLE);
    const weights = computeWeights(events);
    // job test (1): alpha → 3500ms (40s → 43.5s), beta → 4500ms (43.5s → 48s),
    // gamma → 2000ms (48s → 50s), delta dropped (no successor).
    expect(weights.get("tests/unit/alpha.test.ts")).toBe(3500);
    expect(weights.get("tests/unit/beta.test.ts")).toBe(4500);
    expect(weights.get("tests/unit/gamma.test.ts")).toBe(2000);
    expect(weights.get("tests/unit/delta.test.ts")).toBeUndefined();
    // job test (2): epsilon → 13000ms (42s → 55s), zeta dropped.
    expect(weights.get("tests/unit/epsilon.test.ts")).toBe(13000);
    expect(weights.get("tests/unit/zeta.test.ts")).toBeUndefined();
  });

  it("does not cross job boundaries", () => {
    // If we naively diffed across jobs we'd get bogus values when
    // alpha (test 1) was followed by epsilon (test 2) by clock skew.
    const events = parseLog(SAMPLE);
    const weights = computeWeights(events);
    // alpha → beta within same job: 3500ms. Not 2000ms (which would be
    // alpha's stamp to epsilon's stamp across jobs).
    expect(weights.get("tests/unit/alpha.test.ts")).toBe(3500);
  });

  it("takes the max when a file appears with multiple deltas", () => {
    const events = [
      { job: "test (1)", timestampMs: 1000, file: "tests/unit/x.test.ts" },
      { job: "test (1)", timestampMs: 2000, file: "tests/unit/y.test.ts" },
      { job: "test (2)", timestampMs: 3000, file: "tests/unit/x.test.ts" },
      { job: "test (2)", timestampMs: 8000, file: "tests/unit/y.test.ts" },
    ];
    const w = computeWeights(events);
    // x: 1000→2000 (1000ms in job 1), 3000→8000 (5000ms in job 2). Max = 5000.
    expect(w.get("tests/unit/x.test.ts")).toBe(5000);
  });

  it("skips out-of-order events defensively (negative delta)", () => {
    const events = [
      { job: "test (1)", timestampMs: 5000, file: "tests/unit/x.test.ts" },
      { job: "test (1)", timestampMs: 3000, file: "tests/unit/y.test.ts" },
    ];
    const w = computeWeights(events);
    // x → negative delta → skipped. y has no successor → not recorded.
    expect(w.size).toBe(0);
  });

  it("empty input → empty map", () => {
    expect(computeWeights([]).size).toBe(0);
  });
});

describe("serializeWeights", () => {
  it("sorts keys alphabetically for stable diffs", () => {
    const w = new Map([
      ["tests/unit/z.test.ts", 100],
      ["tests/unit/a.test.ts", 200],
      ["tests/unit/m.test.ts", 50],
    ]);
    const json = serializeWeights(w);
    const lines = json.split("\n").filter((l) => l.includes('"tests/unit/'));
    expect(lines[0]).toContain("a.test.ts");
    expect(lines[1]).toContain("m.test.ts");
    expect(lines[2]).toContain("z.test.ts");
  });

  it("ends with a trailing newline (POSIX-friendly diff)", () => {
    const w = new Map([["tests/unit/x.test.ts", 1]]);
    expect(serializeWeights(w).endsWith("\n")).toBe(true);
  });

  it("empty map → empty object JSON", () => {
    expect(serializeWeights(new Map())).toBe("{}\n");
  });

  it("round-trips: parse our own output", () => {
    const w = new Map([
      ["tests/unit/alpha.test.ts", 100],
      ["tests/unit/beta.test.ts", 250],
    ]);
    const json = serializeWeights(w);
    const parsed = JSON.parse(json);
    expect(parsed["tests/unit/alpha.test.ts"]).toBe(100);
    expect(parsed["tests/unit/beta.test.ts"]).toBe(250);
  });
});
