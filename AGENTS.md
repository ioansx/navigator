# nav

A terminal file navigator meant to be launched from inside neovim: it runs in a
floating terminal, and opening a file hands that file to the surrounding neovim
instance over `$NVIM`, then exits. One invocation, one short session — that
assumption is load-bearing in several places, so check it before working around it.

The binary is `nav` (`src/main.rs`); everything else is a library, so it can be
tested without a terminal.

## Layout

- `navigator.rs` — the whole TUI: state, key handling, rendering. The only place
  that knows what a key means.
- `plan.rs` — staged operations, and the checks that need no filesystem. Pure.
- `marks.rs`, `memory.rs` — the paths you selected; where the cursor was in each
  directory you visited. Pure.
- `globals.rs` — colors, icons, scroll constants.
- `log_store.rs` — the session log behind `log::info!` and friends, and the dump
  written to `$TMPDIR/nav-<pid>.log` as soon as anything logs an error.
- `error.rs` — `Errx` / `Resultx`: an error carrying context and a backtrace.
- `io/` — everything that touches the filesystem or spawns a process.

Two external programs, both optional at runtime: `nvim --server` opens files, and
`zoxide query` answers the `z` jump. Missing either one is a warning on the status
line, never a crash.

## What holds the design together

**`io/` is the boundary.** Anything that reads the disk, spawns a process, or
writes escape sequences to the terminal lives under it and hands back plain data,
so the rest of the crate renders without knowing how anything was read. Where a
check needs both halves, the pure half stays outside: `Op::problem` decides what
can be known from the paths alone, `fs_ops::problem` asks the disk the rest.

**Nothing touches the disk until a plan is applied.** Keys stage `Op`s, `p`
reviews them, `enter` applies. Applying runs every operation even after one fails
— a filesystem has no transactions, so stopping halfway would leave no account of
which half ran — and whatever failed stays staged with its reason attached.

**Deletes go to the trash**, `$TMPDIR/nav-trash/<pid>/`, mirroring the original
tree so two files of the same name cannot collide.

**The session is short.** Cursor memory lives in the process, the log keeps every
entry, the terminal's image protocol is queried once at startup. Anything that
should outlive the process is a new feature with its own storage and staleness
questions, not a tweak to one of these.

**`Q` moves the shell, `q` does not.** A process cannot `cd` its parent, so `Q`
writes where the session ended to `--cwd-file <path>` and the shell function in
`~/.ioansx/fish/config.fish` follows it there. Every other way out — `q`, or
handing a file to neovim — writes nothing, and the wrapper finds an empty file
and stays put:

```fish
function nav --wraps nav
    set -l cwd_file (mktemp -t nav-cwd)
    command nav --cwd-file $cwd_file $argv
    set -l ended (cat $cwd_file)
    rm -f $cwd_file

    if test -d "$ended"; and test "$ended" != $PWD
        cd $ended
    end
end
```

## Conventions

**Tests live beside what they test**, in `#[cfg(test)] mod tests`, named as a
sentence about behaviour: `going_up_highlights_the_directory_you_came_from`. There
are no test-only dependencies — `io/testdir.rs` gives you a `TempDir`, and
rendering is checked by drawing into a `Buffer` and asserting on the text it
produced (`render_to_string` in `navigator.rs`).

**Comments explain why, never what.** The ones worth writing are the ones that
save a reader from an obvious-looking "fix": why cursor positions are stored as
entry names rather than row numbers, why quotes are doubled in the vimscript, why
`EXDEV` is spelled out by number. Pain in the code is fixed by restructuring it,
not by describing it.

**Colors come from the terminal's 16-color palette.** No `Color::Rgb` or
`Color::Indexed` anywhere, so `nav` follows whatever theme the emulator is set to;
a test in `globals.rs` enforces this.

**Every dependency is `default-features = false`**, with a comment in `Cargo.toml`
naming the features that are on and why. Adding a crate means justifying it there.

**Keys are documented in `HELP` in `navigator.rs`**, kept directly above the
handler they describe. A test asserts every documented key reaches the screen, so
add the help entry with the binding, not after it.

## Commands

```sh
cargo test
cargo clippy --all-targets   # pedantic + nursery + cargo, all deny — keep it clean
cargo fmt
```

`nav` needs a tty, so a smoke run is `script -q /dev/null ./target/debug/nav <dir>`.
`RUST_LOG=debug` turns on the detail; `L` opens the log panel in-app.

Commit messages are short and lowercase: `zoxide support`, `file ops`.
