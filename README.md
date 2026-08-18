# m1-can

Inspect a MoTeC M1 project's actual CAN topology, export its `.m1dbc`
databases as standard Vector `.dbc` files, and tell CI when an export is stale.

A `.m1dbc` declares messages but carries no bus. `m1-can inspect` combines the
databases with the project's `DBC.<Name>.Init(<bus>)` calls and calibration to
classify repeated IDs as real same-bus clashes, safe different-bus reuse, or
unknown. `m1-can export` renders each database as a standard Vector `.dbc`
so Vector tools, SavvyCAN, python-can and the rest of the paddock can open the
same database. The `.m1dbc` stays the source of truth; the `.dbc` is a generated
artefact that happens to be committed.

It is one of the M1 toolchain's sibling CLIs, alongside
[m1-lsp](https://github.com/C-Nucifora/m1-lsp),
[m1-fmt](https://github.com/C-Nucifora/m1-fmt),
[m1-lint](https://github.com/C-Nucifora/m1-lint),
[m1-typecheck](https://github.com/C-Nucifora/m1-typecheck) and
[m1-project](https://github.com/nedlane/m1-project). Export and inspection live
together here so every consumer uses one CAN model.

## Install

Prebuilt binaries for Linux, macOS, and Windows are attached to each
[release](https://github.com/nedlane/m1-can/releases). Or build from source:

```sh
cargo install --git https://github.com/nedlane/m1-can.git --tag <latest>
```

## Usage

```sh
m1-can inspect --project UQR-EV/01.00/Project.m1prj
m1-can export           # regenerate the .dbc exports
m1-can export --check   # CI/hook: are they up to date? (writes nothing)
```

`inspect` emits structured JSON. `--filter TEXT` narrows its returned message
list and `--limit N` caps that list without changing the overlap analysis.
The library exposes the same `inspect` API for in-process consumers such as
`m1-mcp`; no installed CLI or `PATH` lookup is involved.

## CAN topology and overlap verdicts

CAN identifiers are scoped to a bus. Two modules may legitimately reuse an ID
when their `Init` calls bind them to different buses. `inspect` resolves literal
buses, project constants, and parameter values from `parameters.m1cfg`, and
marks verdicts that depend on calibration because a retune can change them.
Uninitialised modules and unresolved expressions remain `unknown` rather than
being guessed.

## Configuration

`m1-can export` takes no path argument. It is configured by the `[dbc]` section of the
`m1-tools.toml` that governs the working directory — found by the same walk-up
every M1 tool uses — so it can be run from anywhere in the repo, including from
a hook that knows nothing about paths:

```toml
[dbc]
src_dir = "UQR-EV/01.00/dbc"   # where the .m1dbc sources live
out_dir = "dbc"                # where the .dbc exports go

# source .m1dbc stem -> output .dbc stem
[dbc.exports]
"Balls3EV25" = "Balls3EV25"
"SBG DBC" = "SBG"
```

Both directories are paths relative to the directory holding the
`m1-tools.toml`. The stem mapping is explicit rather than derived: an M1-Build
module often carries a ` DBC` suffix so it does not collide with another project
object, and the exported file should not — but it is not a general "strip a
trailing ` DBC`" rule (`PDM P14` keeps its space), so each repo states the pairs
it wants.

A repo with no `[dbc]` section — or no `m1-tools.toml` at all — is a clean no-op,
not an error.

Both stems in a pair are bare file names; `src_dir` and `out_dir` are the only
places a directory is named. No two pairs may share an output stem, or one
export would silently overwrite another. Empty, absolute or `..`-bearing values
are refused with exit 2 rather than guessed at, as is an empty or absolute
`src_dir`/`out_dir`.

The `[dbc]` section is invisible to the shared `m1-tools.toml` schema, so adding
it does not disturb m1-fmt, m1-lint, m1-typecheck or the LSP.

## Checking (`--check`)

`--check` writes nothing into the working tree: it generates into a temporary
directory and compares against the committed exports. The comparison is on
**decoded text, not bytes**, so three things are deliberately *not* drift:

- **line endings** — the exports are usually `eol=crlf` in git while the writer
  emits LF;
- **a leading UTF-8 BOM** on the committed file;
- **a committed export stored as Windows-1252** rather than UTF-8.

Only a difference in the text itself fails the check. A committed export that
does not exist yet compares as empty, so the first run in a repo reports "out of
sync" rather than failing.

That is the CI gate and the pre-commit hook — it catches a `.m1dbc` edited
without regenerating, which is how an export silently goes stale.

## Self-verification

Every run verifies its own work, so a bad export fails loudly rather than
shipping:

- the messages and signals **written** must equal the **convertible** count
  taken by an independent second walk of the source; and
- re-reading the written file with a **third-party DBC parser** (`can-dbc`) must
  reproduce those same counts.

The report names every skipped component and why — the `VECTOR INDEPENDENT SIG
MSG` container and its orphan signals, a message with no `CANId`, and the file's
own `BuiltIn.CAN.DBC` metadata component:

```
=== SBG ===
  source .m1dbc   : 69 messages, 194 signals
  convertible     : 69 messages, 194 signals (source minus skipped)
  written .dbc    : 69 messages, 194 signals
  round-tripped   : 69 messages, 194 signals
  skipped         : 1
    - BuiltIn.CAN.DBC 'SBG DBC' (not a CAN frame)
  in sync         : yes
```

## Exit codes

| Code | Meaning |
| --- | --- |
| `0` | Written; or with `--check`, everything in sync. Also a repo with no `[dbc]` section — nothing to export is not a failure. |
| `1` | A count or round-trip mismatch, or a committed export that is out of date. The tool worked; the files do not match. |
| `2` | The config, a source, or a generated file could not be read or parsed. Nothing about the affected pair can be trusted, including "in sync". |

The 0/1 split is the toolchain's usual "no findings / findings" convention; `2`
is a deliberate extension so a CI gate can tell a **stale export** (regenerate
and commit) from a **broken setup** (fix the config) without parsing the report.

A failing file does not stop the run: every file is reported and the worst code
is returned.

## Determinism, and the one regeneration commit

Output is **deterministic** — same source, same bytes, on every machine and in
any order. Messages come out in `.m1dbc` document order and each message's
signals in the order that file lists them; nothing is sorted, hashed into the
output, or read from the environment. So a regenerated export is either
identical or a real change, and a diff is worth reading.

The layout is **this tool's own canonical format**. Reproducing the older
Python/cantools pipeline's bytes is an explicit **non-goal** — nothing is
invented (the min/max field is always `[0|0]`, because MoTeC stores neither),
scale and offset use shortest round-trip float formatting so an `f32`-precision
multiplier survives intact, and identifiers longer than 32 characters are
written whole rather than shortened into a long-symbol table.

**Adopting `m1-can` in a repo whose `.dbc` files were generated by something
else therefore needs one regeneration commit**: run `m1-can export` once, commit
the reformatted exports — the *content* is unchanged, same frames, same signals
— and every run after that is a no-op until a `.m1dbc` actually changes.

## Development

The CI gate is `cargo test`, `cargo clippy --all-targets -- -D warnings`, and
`cargo fmt --all -- --check`, plus rustdoc with `-D warnings`, a security audit,
and an MSRV job pinned to the same toolchain as `rust-version` in `Cargo.toml`.
See [AGENTS.md](AGENTS.md) for what is deliberate in here and must not be
"fixed".

## License

GPL-3.0-or-later — see [LICENSE](LICENSE).

## Trademark

Independent, community-built open-source tooling for the MoTeC® M1 script
language. Not affiliated with, authorised, or endorsed by MoTeC Pty Ltd.
"MoTeC" and "M1" are trademarks of MoTeC Pty Ltd.
