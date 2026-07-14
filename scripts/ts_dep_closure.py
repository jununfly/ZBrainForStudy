#!/usr/bin/env python3
"""TS import dependency-graph analyzer for safe deletion planning.

Builds a directed import graph over all tracked .ts files, then given a set of
KEEP seed files, computes the *downstream transitive closure* R (everything the
seeds import, transitively). The safe-deletion set is D = ALL - R: deleting D
cannot dangle any import that a kept file relies on, so `tsc --noEmit` over the
kept set stays sound.

Usage:
  ts_dep_closure.py graph            # emit graph stats
  ts_dep_closure.py closure <seedfile>  # seedfile = newline list of KEEP paths (globs ok)
  ts_dep_closure.py importers <path>    # who imports <path> (reverse edges)
"""
import sys, os, re, json, glob, fnmatch

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

# typecheck scope per tsconfig include: src, tests/unit. scripts/ and tests/heavy
# are OUTSIDE tsc --noEmit, so they never affect the typecheck gate.
SCAN_DIRS = ["src", "tests/unit", "tests/heavy", "scripts"]

IMPORT_RE = re.compile(
    r"""(?:import|export)\s+(?:[^;'"]*?\bfrom\s+)?['"]([^'"]+)['"]"""
    r"""|require\(\s*['"]([^'"]+)['"]\s*\)"""
    r"""|import\(\s*['"]([^'"]+)['"]\s*\)""",
    re.M,
)


def list_ts_files():
    files = []
    for d in SCAN_DIRS:
        for ext in ("ts", "tsx"):
            files.extend(
                glob.glob(os.path.join(ROOT, d, "**", f"*.{ext}"), recursive=True)
            )
    # normalize to repo-relative posix
    out = set()
    for f in files:
        rel = os.path.relpath(f, ROOT).replace("\\", "/")
        out.add(rel)
    return out


# Ambient declaration files (.d.ts) are referenced implicitly by the compiler,
# not via import edges, so the import-graph closure cannot see their consumers.
# They must NEVER be auto-classified as deletable. (Learned the hard way:
# deleting src/types/image-decoders.d.ts broke src/core/import-file.ts typing.)
def is_ambient(f):
    return f.endswith(".d.ts")


def resolve(spec, from_file, allset):
    """Resolve an import specifier to a repo-relative .ts path, or None if external."""
    if spec.startswith("@/"):
        base = "src/" + spec[2:]
    elif spec.startswith("."):
        base = os.path.normpath(
            os.path.join(os.path.dirname(from_file), spec)
        ).replace("\\", "/")
    else:
        return None  # bare module (node_modules / builtin)
    # strip a trailing .js/.ts and try candidates
    cands = []
    stem = re.sub(r"\.(ts|tsx|js|jsx)$", "", base)
    for ext in (".ts", ".tsx"):
        cands.append(stem + ext)
    for ext in ("/index.ts", "/index.tsx"):
        cands.append(stem + ext)
    # also literal (already had extension that exists)
    cands.append(base)
    for c in cands:
        if c in allset:
            return c
    return None


def build_graph():
    allset = list_ts_files()
    fwd = {f: set() for f in allset}  # f imports -> set
    rev = {f: set() for f in allset}  # f is imported by -> set
    unresolved = []
    for f in allset:
        try:
            txt = open(os.path.join(ROOT, f), encoding="utf-8", errors="replace").read()
        except OSError:
            continue
        for m in IMPORT_RE.finditer(txt):
            spec = m.group(1) or m.group(2) or m.group(3)
            if not spec:
                continue
            tgt = resolve(spec, f, allset)
            if tgt is None:
                if spec.startswith(".") or spec.startswith("@/"):
                    unresolved.append((f, spec))
                continue
            fwd[f].add(tgt)
            rev[tgt].add(f)
    return allset, fwd, rev, unresolved


def _pat_to_re(p):
    """Convert a glob with ** (recursive) / * (single segment) to a regex."""
    out = []
    i = 0
    while i < len(p):
        c = p[i]
        if p[i : i + 3] == "**/":
            out.append("(?:.*/)?")  # zero-or-more path segments
            i += 3
        elif p[i : i + 2] == "**":
            out.append(".*")
            i += 2
        elif c == "*":
            out.append("[^/]*")
            i += 1
        elif c == "?":
            out.append("[^/]")
            i += 1
        else:
            out.append(re.escape(c))
            i += 1
    return re.compile("^" + "".join(out) + "$")


def expand_seeds(patterns, allset):
    seeds = set()
    for p in patterns:
        p = p.strip()
        if not p or p.startswith("#"):
            continue
        if p in allset:
            seeds.add(p)
            continue
        rx = _pat_to_re(p)
        matched = [f for f in allset if rx.match(f)]
        if matched:
            seeds.update(matched)
        else:
            print(f"  [warn] seed pattern matched nothing: {p}", file=sys.stderr)
    return seeds


def downstream_closure(seeds, fwd):
    seen = set(seeds)
    stack = list(seeds)
    while stack:
        n = stack.pop()
        for t in fwd.get(n, ()):
            if t not in seen:
                seen.add(t)
                stack.append(t)
    return seen


def main():
    cmd = sys.argv[1] if len(sys.argv) > 1 else "graph"
    allset, fwd, rev, unresolved = build_graph()
    if cmd == "graph":
        print(f"total ts files: {len(allset)}")
        edges = sum(len(v) for v in fwd.values())
        print(f"import edges (resolved): {edges}")
        print(f"unresolved relative imports: {len(unresolved)}")
        for f, s in unresolved[:25]:
            print(f"  {f} -> {s}")
    elif cmd == "importers":
        tgt = sys.argv[2].replace("\\", "/")
        print(f"importers of {tgt}:")
        for f in sorted(rev.get(tgt, ())):
            print(f"  {f}")
    elif cmd == "closure":
        seedfile = sys.argv[2]
        pats = open(seedfile, encoding="utf-8").read().splitlines()
        seeds = expand_seeds(pats, allset)
        # Iterative fixpoint: a KEPT test (one that imports any retained impl) also
        # pins everything IT imports. Otherwise deleting a D-impl would dangle a kept
        # test's import and break tsc. Repeat until R stops growing.
        R = downstream_closure(seeds, fwd)
        while True:
            D = allset - R
            # kept tests = test files NOT fully-deletable = import >=1 file in R-impl
            grew = False
            for t in list(allset):
                if not (".test." in t or t.startswith("tests/")):
                    continue
                if t in R:
                    continue
                impl_imports = [
                    x for x in fwd.get(t, ())
                    if (".test." not in x and not x.startswith("tests/"))
                ]
                # a test is KEPT if it imports any retained impl
                if any(x in R for x in impl_imports):
                    add = downstream_closure({t}, fwd)
                    if not add <= R:
                        R |= add
                        grew = True
            if not grew:
                break
        D = (allset - R) - {f for f in allset if is_ambient(f)}
        print(f"seeds (KEEP roots): {len(seeds)}")
        print(f"R (keep closure = seeds + everything they import): {len(R)}")
        print(f"D (safe-delete = ALL - R, ambient .d.ts excluded): {len(D)}")
        # split D by area
        def area(f):
            if f.startswith("scripts/"): return "scripts"
            if f.startswith("tests/heavy/"): return "tests/heavy"
            if f.startswith("tests/unit/"): return "tests/unit"
            if f.startswith("src/commands/"): return "src/commands"
            if f.startswith("src/mcp/"): return "src/mcp"
            if f.startswith("src/core/"):
                parts = f.split("/")
                return "src/core/" + (parts[2] if len(parts) > 3 else "*")
            if f.startswith("src/eval/"): return "src/eval"
            return f.split("/")[0]
        from collections import Counter
        c = Counter(area(f) for f in D)
        print("\n=== D breakdown by area ===")
        for a, n in sorted(c.items(), key=lambda x: -x[1]):
            print(f"  {n:4d}  {a}")
        # dump full D + R to a temp dir (scratch artifacts, not committed)
        import tempfile
        tmp = tempfile.gettempdir()
        out = os.path.join(tmp, "ts_delete_set.txt")
        open(out, "w", encoding="utf-8").write("\n".join(sorted(D)) + "\n")
        print(f"\nfull D written to {out}")
        outR = os.path.join(tmp, "ts_keep_set.txt")
        open(outR, "w", encoding="utf-8").write("\n".join(sorted(R)) + "\n")
        print(f"full R written to {outR}")
    elif cmd == "tests":
        # classify tests: a test is DELETE-WITH-SLICE iff every non-test impl file
        # it imports is in D (safe-delete). A test that imports any kept (R) impl
        # must be KEPT (it still covers retained behavior) or rewritten.
        seedfile = sys.argv[2]
        pats = open(seedfile, encoding="utf-8").read().splitlines()
        seeds = expand_seeds(pats, allset)
        R = downstream_closure(seeds, fwd)
        D = allset - R
        testfiles = [f for f in allset if ".test." in f or f.startswith("tests/")]
        del_tests, keep_tests, orphan_tests = [], [], []
        for t in testfiles:
            impl_imports = [
                x for x in fwd.get(t, ()) if (".test." not in x and not x.startswith("tests/"))
            ]
            if not impl_imports:
                orphan_tests.append(t)
            elif all(x in D for x in impl_imports):
                del_tests.append(t)
            else:
                keep_tests.append(t)
        print(f"tests total: {len(testfiles)}")
        print(f"  DELETE-WITH-SLICE (all impl imports in D): {len(del_tests)}")
        print(f"  KEEP (imports >=1 retained impl): {len(keep_tests)}")
        print(f"  ORPHAN (no impl import / self-contained): {len(orphan_tests)}")
        which = sys.argv[3] if len(sys.argv) > 3 else None
        if which == "del":
            print("\n".join(sorted(del_tests)))
        elif which == "keep":
            print("\n".join(sorted(keep_tests)))
        elif which == "orphan":
            print("\n".join(sorted(orphan_tests)))


if __name__ == "__main__":
    main()
