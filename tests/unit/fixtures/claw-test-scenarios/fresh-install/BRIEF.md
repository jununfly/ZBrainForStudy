# Claw-test brief — fresh-install

You are testing zbrain on a brand-new install. The user just ran `zbrain init` for the first time. Walk through the canonical first-day flow:

1. **Verify install:** confirm `zbrain --version` works and `zbrain doctor --json` returns a valid JSON object with a `status` field.
2. **Install skillpack:** run `zbrain skillpack install --workspace $PWD`. The workspace already has an `AGENTS.md` routing file.
3. **Import the brain:** run `zbrain import ./brain --no-embed --progress-json`. There are 3 small markdown pages already there.
4. **Query the brain:** run `zbrain query "alice"` and verify >0 results.
5. **Extract links:** run `zbrain extract --source fs --progress-json`.
6. **Verify health:** run `zbrain doctor --json`. The `status` field should be `"ok"`.

## Friction protocol

If anything is confusing, missing, surprising, or wrong, run:

```
zbrain friction log --severity {confused|error|blocker|nit} --phase <which-step> --message "<what-happened>" [--hint "<what-could-be-better>"]
```

Severity guide:
- `blocker` — couldn't proceed at all
- `error` — command failed unexpectedly
- `confused` — docs said one thing, the tool did another, or a step felt unclear
- `nit` — minor polish opportunity

If something *just worked* and was nicer than expected, log a delight too:

```
zbrain friction log --kind delight --phase <step> --message "<what-was-nice>"
```

We want to know what didn't work, not just whether commands exited zero. Be specific.
