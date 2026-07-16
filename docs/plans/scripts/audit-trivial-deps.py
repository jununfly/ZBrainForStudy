"""Precise PARITY_GATE audit for 1-6-3 TRIVIAL_DELETE candidates.

For each candidate:
  - in_cli_only: is it in the CLI_ONLY set?
  - modules: command module files it imports in cli.ts dispatch
  - src_refs: real `import ... from '<module>'` / `import('<module>')` in src/ (excluding cli.ts)
  - test_refs: references in tests/ (import of module OR spawn of the command)

Run from repo root: python docs/plans/scripts/audit-trivial-deps.py
"""
import re
import subprocess

# The 27 audit-doc TRIVIAL_DELETE entries. discovery/network/parse are NOT real
# commands (they are RemoteMcpError.reason switch cases) — flagged separately.
CANDIDATES = ['anomalies', 'apply-migrations', 'book-mirror', 'cache', 'call', 'check-backlinks',
              'check-update', 'claw-test', 'discovery', 'files', 'founder', 'friction', 'frontmatter',
              'integrations', 'lint', 'lsd', 'mounts', 'network', 'parse', 'post-upgrade', 'reinit-pglite',
              'repair-jsonb', 'report', 'smoke-test', 'transcripts', 'upgrade', 'ze-switch']

cli = open('src/cli.ts', encoding='utf-8').read()
cli_only_line = cli.splitlines()[37]
cli_only = set(re.findall(r"'([^']+)'", cli_only_line))


def modules_for(cmd):
    """Find command module(s) imported in cli.ts for this command's dispatch."""
    mods = set()
    # match `if (command === 'cmd')` block or `case 'cmd':` block
    for pat in [r"command === '" + re.escape(cmd) + r"'[^\n]*\)\s*\{(.*?)\n  \}",
                r"case '" + re.escape(cmd) + r"':\s*\{(.*?)break;"]:
        for m in re.finditer(pat, cli, re.DOTALL):
            for mi in re.finditer(r"import\('(\./commands/[^']+)'\)", m.group(1)):
                mods.add(mi.group(1))
    return mods


def grep(pattern, path):
    r = subprocess.run(['git', 'grep', '-n', '-e', pattern, '--', path],
                       capture_output=True, text=True)
    return [l for l in r.stdout.splitlines() if l.strip()]


print(f"{'CMD':<26} {'inSet':<6} {'modules'}")
print("-" * 90)
all_mods = {}
for cmd in CANDIDATES:
    in_set = 'Y' if cmd in cli_only else 'NO-CMD'
    mods = modules_for(cmd)
    all_mods[cmd] = mods
    print(f"{cmd:<26} {in_set:<6} {sorted(mods)}")

print("\n=== src refs (real imports outside cli.ts) + test refs ===")
for cmd in CANDIDATES:
    mods = all_mods[cmd]
    print(f"\n[{cmd}]")
    if not mods:
        print("   (no dispatch module — inline handler or non-command)")
    for mod in sorted(mods):
        base = mod.replace('./', '').replace('.ts', '')  # commands/xyz
        # real import references
        src = grep(f"from '\\./{base}'", 'src/') + grep(f"import('\\./{base}", 'src/')
        src += grep(f"from '\\.\\./{base}'", 'src/') + grep(f"from '\\.\\./\\.\\./{base}'", 'src/')
        src = [l for l in src if 'src/cli.ts:' not in l]
        tst = grep(base, 'tests/')
        print(f"   module {mod}:")
        print(f"     src_refs (excl cli.ts): {len(src)}")
        for l in src[:6]:
            print(f"       {l}")
        print(f"     test_refs: {len(tst)}")
        for l in tst[:6]:
            print(f"       {l}")
