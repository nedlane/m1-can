# AGENTS.md — m1-dbc

Guidance for coding agents working in this repository.

## Purpose

One job: turn a repo's MoTeC `.m1dbc` CAN databases into standard Vector `.dbc`
files, and tell CI whether the committed exports are still in sync. It was
extracted from `m1-project`, which keeps everything about editing
`Project.m1prj` — including `validate`'s DBC checks. Nothing about `.m1prj`
belongs here.

The CLI contract (subcommand, flags, report text, exit codes) is a public API:
the downstream pre-commit hook and the m1-ci gate call `m1-dbc export --check`
and branch on its exit code, so changing any of it breaks them.

## Things that are deliberate (don't "fix" them)

- **The writer defines this tool's own canonical `.dbc` layout.** It is not
  reproducing cantools', or any other library's, output byte-for-byte — byte
  parity with the older Python/cantools pipeline is an explicit **non-goal**.
  What is guaranteed is determinism (same source, same bytes, any machine, any
  order) and that the *content* — frames, signals, scaling — is unchanged.
- **DIVERGENCE comments are the specification record.** Where this
  implementation departs from the normative Python reference (the `.//`
  descendant search, the tolerant decode, the second skip rule in
  `count_source`), the departure is written down at the site with its reason.
  Keep them; they are how a reviewer checks the port.
- **A second, independent walk counts the source.** `count_source` re-derives
  the skip rules from scratch rather than measuring the parsed model, so
  `written == convertible` is a real cross-check and not a tautology. And every
  run re-reads what it wrote through the third-party `can-dbc` crate, so the
  output is judged by code this repo did not write.
- **Real `.m1dbc` files are Windows-1252 in practice** — a bare `0xB0` for the
  degree sign, with no declared XML encoding. All reads/writes go through
  `m1-workspace`'s tolerant decode and atomic-write helpers, never raw
  `fs::read_to_string` / in-place truncating writes. The committed fixture is
  pinned `-text` in `.gitattributes` so git never rewrites its bytes.
- **The `.dbc` is written UTF-8 with LF endings**, not Windows-1252: a `.dbc` is
  a Vector file for non-MoTeC tools, not a MoTeC file. `--check` normalises line
  endings and reads both sides through the tolerant decode, so CRLF, a leading
  BOM, and a committed export stored as Windows-1252 are deliberately **not**
  drift.
- **`--check` never touches the working tree.** It generates into a fresh temp
  directory and compares. That is why config values are validated so hard: an
  absolute or `..`-bearing export stem would let `Path::join` discard the
  scratch base and write into the repo — and then compare a file against
  itself.
- **Refuse rather than guess.** An unparsable number, a component with no
  `<Props>`, a half-understood `[dbc]` section — all hard errors naming the
  file and the key. An *absent* `[dbc]` section is not an error: it is a clean
  no-op, so the tool is shippable to repos that export no DBCs.
- **The exit convention is 0/1/2.** 0/1 is the toolchain's usual "no findings /
  findings"; **2** is a deliberate extension so a gate can tell a stale export
  (regenerate and commit) from a broken setup (fix the config) without parsing
  the report.
- **The public API is three items** — `export`, `Outcome`, `ExportError`. The
  four stage modules are private on purpose. Don't republish internals to make
  a test easier; unit tests live inside the module they test.

## Build / test gate

```sh
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --all -- --check
```

CI also runs rustdoc with `-D warnings`, a security audit, and an MSRV job.
The MSRV pin in CI (`dtolnay/rust-toolchain@<version>`) must stay in sync with
`rust-version` in `Cargo.toml` — never bump one without the other.

Behaviour changes should be checked against a real corpus (the EV-M1/AV-M1
firmware repos) before being trusted: the message/signal counts per export are
the regression signal.

## Dependencies and releases

Depends on `m1-workspace` via a **versioned git tag** — never
`branch`/`path`/`[patch]`; the repo must build exactly like a public clone.
This is a binary repo: a version bump on `main` makes `release.yml` tag it and
upload prebuilt binaries with build provenance attestations. m1-ci verifies that
provenance with `gh attestation verify` and refuses a binary it cannot verify,
so releases must come from the workflow — never hand-uploaded assets.
