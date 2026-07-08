# VolentScript examples

*Back to the [main README](../README.md).*

Each directory is a self-contained program (usage in its file header).
Build any of them with `volentscript build <file> -o <name>`, or run
directly with `volentscript run <file> [args]`.

| Example | What it is | What it shows off |
|---|---|---|
| [`life/`](life) | Conway's Game of Life in the terminal | `Vector.<T>`, classes, numeric loops |
| [`todo/`](todo) | JSON-backed task manager | JSON round-trips, File IO, args subcommands, `Date.now()` |
| [`vgrep/`](vgrep) | grep clone | RegExp, recursive `File.list`, stdin filtering, exit codes |
| [`calc/`](calc) | arithmetic REPL | a recursive-descent parser *written in VolentScript*, exceptions |
| [`logstats/`](logstats) | access-log analyzer | RegExp capture groups, dynamic objects as maps, `sort` comparators |
| [`httpd/`](httpd) | static-file web server | `ServerSocket`, HTTP parsing over `readLine`, content types |
| [`mail/`](mail) | mini SMTP server + client | HELO/EHLO wire protocol, session state, mail filed to disk |
| [`chat/`](chat) | 1:1 terminal chat | turn-based bidirectional sockets + stdin |

Every example is compiled and exercised by `cargo test -p e2e --test examples`
so they can't rot.

## Running them

```sh
cargo run -p volentscript -- run examples/life/main.vlt        # from a checkout
volentscript run examples/calc/main.vlt                        # from a release
```

Servers print their bound port on startup (`0` = pick an ephemeral one),
so `run examples/httpd/main.vlt 0 examples/httpd/public` always works.

## Scope notes

The v1 runtime is single-threaded and blocking — the servers handle one
connection/session at a time (fine for tools and demos). All IO is text
(UTF-8); there is no TLS.
