# Zyris

An expandable computer protocol. A **Zyris node** is any machine that dials a server over one
websocket, announces typed **capabilities** — named, versioned sets of tools with JSON-Schema'd
arguments — and, on the same connection, consumes the capabilities the server announces back.

The protocol is direction-symmetric. There is no client role and no server role once the handshake
is done: either peer may call the other, open streams, and change what it offers mid-session.

[Attacca](https://attacca.cc) is the reference deployment: a node's tools become tools its owner's
agents can call, and the server announces `attacca_api` so the same node can drive agents and
sessions in return.

## The crates

| Crate | What it is |
|---|---|
| [`zyris`](crates/zyris) | The node runtime and client — connection state machine, transports, capability announce/accept, the dial/reconnect loop, and device-grant enrollment. Depend on this one. |
| [`zyris-proto`](crates/zyris-proto) | Wire types only: envelopes, frames, the handshake, datums and blobs. No I/O, no async. Re-exported as `zyris::proto`. |
| [`zyris-macros`](crates/zyris-macros) | The `#[zyris::capability]` proc-macro. Re-exported by `zyris`; not a direct dependency. |
| [`zyris-caps`](crates/zyris-caps) | The standard capability catalogue — `terminal`, `file_io`, `input`, `screen_capture`, `browser_chrome`. Declarations only: no tokio, no OS dependencies, cheap for a client to depend on. |
| [`zyris-capkit`](crates/zyris-capkit) | Reference implementations of that catalogue: `LocalFileIo` and `PtyTerminal` by default, plus `XcapScreenCapture` and `EnigoInput` behind the `screen` and `input` features. |
| [`zyris-attacca`](crates/zyris-attacca) | The `attacca_api` capability: the one surface that runs the other way, announced by the server rather than by a node. Depend on it to call Attacca back. |
| [`zyris-hello`](crates/zyris-hello) | A complete node in two short files. The thing to copy. |

## A capability

One trait. The macro turns it into the descriptor, a server wrapper, and a client:

```rust
#[zyris::capability(name = "hello", version = 1)]
pub trait Hello {
    /// Return a random friendly greeting, optionally addressed to `name`.
    async fn greet(&self, name: Option<String>) -> zyris::Result<Greeting>;
}
```

Doc comments become the tool and field descriptions a model reads, so write them for the model.

## Running the reference node

```bash
cargo run -p zyris-hello
```

With nothing configured it enrolls itself against `attacca.cc`: it prints an 8-character code, you
type that into Attacca on whatever device has a browser, and it connects. Point it elsewhere with
`ZYRIS_SERVER_URL`. See [`crates/zyris-hello/README.md`](crates/zyris-hello/README.md) for the full
configuration table and what to copy.

## Documentation

[`docs/zyris-protocol.md`](docs/zyris-protocol.md) is the normative wire reference: framing,
envelopes, the connection lifecycle, streams and flow control, capabilities, datums, video, and the
close codes. Read `zyris-hello` first; read the spec when you need to know exactly what the bytes
mean.

## License

MIT or Apache-2.0, at your option.
