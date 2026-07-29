# zyris-hello

The smallest complete Zyris node, and the reference to copy when writing your own. It announces one
capability (`hello`) with one tool (`greet`, which returns a random greeting) and consumes the
`attacca_api` capability Attacca announces back — both over the same websocket.

It depends on no `attacca-*` crate. That is deliberate: a node needs only the Zyris stack. The
`attacca_api` capability it consumes comes from `zyris-attacca`, which is part of that stack and
carries the declaration alone — no server, no database, no deployment.

## Running it

Against the hosted deployment there is nothing to configure at all — no URL, no token, no name:

```bash
cargo run -p zyris-hello
```

Against a local server, point it somewhere else:

```bash
# 1. Attacca, in one terminal
cargo run -p attacca-server

# 2. This node, in another terminal. Still no token needed.
export ZYRIS_SERVER_URL=ws://127.0.0.1:8080/zyris/v1/ws
cargo run -p zyris-hello
```

With no token configured, the node names itself after this machine and enrolls itself. It prints a
short code and waits:

```
--------------------------------------------------------------
  Authorize this node

  1. Open        http://127.0.0.1:5173/settings/zyris/device
  2. Enter code  WXQR-7KBD

  Waiting for approval. This code expires in 10 minutes.
  Press Ctrl-C to cancel.
--------------------------------------------------------------
```

Type that code into Attacca on whatever device has a browser — your laptop, your phone, the machine
you SSH'd from. You will see what this node says it is, where Attacca saw the request come from, and
which scopes it asked for; you choose which to grant. Press Authorize and the node connects:

```
Authorized as "build-box" in the account you@example.com. (scopes: agents:read)
INFO zyris::runtime::runner: connected node_id=... conn_id=...
INFO zyris_hello: server announced attacca_api; this node can call back into Attacca
INFO zyris_hello: attacca_api.list_agents ok count=3 first=Researcher
```

Credentials are written to `~/.config/zyris/` (mode `0600`) and refresh themselves, so subsequent
runs connect without printing anything. That is `FileCredentialStore`, the default behind the
`persistence` feature; pass your own `CredentialStore` to `Enroller::new` to keep them in a keychain
or a Secret instead. `ATTACCA_PUBLIC_URL` must be set on the server for the verification URL to be
printable; without it the device endpoints report themselves unavailable rather than printing an
address that goes nowhere.

The node's card in the dashboard flips to connected and lists `hello v1 > greet`. Ask an agent to
call `zyris__{slug}__hello__greet` with `{"name": "Ada"}` — the slug is on the node card, derived
from the name you approved — and it will answer from this process. `Ctrl-C` closes the connection
gracefully and the card flips back to offline.

## Configuration

| Variable | Default | Notes |
|---|---|---|
| `ZYRIS_SERVER_URL` | `zyris::DEFAULT_SERVER_URL` (`wss://attacca.cc/api/zyris/v1/ws`) | Point at a local server with `ws://127.0.0.1:8080/zyris/v1/ws`. The enrollment endpoints are derived from this by truncating at `/zyris/`, so a path prefix like `/api` is carried along and a node cannot enroll against one deployment while connecting to another. |
| `ZYRIS_NODE_TOKEN` | *unset* | A static `znt_` token. Set-but-not-`znt_`-prefixed is a hard error rather than a fall-through, so a mispasted `atk_` API key says so instead of silently enrolling past the diagnostic. |
| `ZYRIS_NODE_TOKEN_FILE` | *unset* | Read the token from a file instead, re-read on every dial so a rotated k8s Secret or systemd `LoadCredential=` needs no restart. Lower precedence than `ZYRIS_NODE_TOKEN`; with neither set the node **enrolls**. |
| `ZYRIS_NODE_NAME` | this machine's hostname | The name proposed at enrollment, which the approving user may change. If you already have a node by that name the server appends `-2`, so two machines called `build-box` stay individually addressable instead of one silently shadowing the other. After enrollment the name is local only: the name and slug agents see come from the dashboard, so renaming here does **not** rename an existing node. |
| `ZYRIS_SCOPES` | what the node asks for in code (`agents:read` here) | Comma-separated scopes to *request*. The user can grant fewer, including none. Setting it wins over `Runner::request_scopes`: an operator deciding what a node may ask for outranks the node's own default. |
| `ZYRIS_PROFILE` | `default` | Names the credential file, so one machine can hold separate identities against the same deployment. |
| `ZYRIS_CONFIG_DIR` | XDG default | Where credentials live, for the default `FileCredentialStore`. Required under `systemd` with `ProtectHome=yes`, where there is no usable `$HOME` — the node fails loudly rather than writing a secret into its working directory. |
| `RUST_LOG` | `zyris_hello=info,zyris=info` | Standard `tracing` filter. The authorization block is printed to stdout, not through `tracing`, so it survives `RUST_LOG=error`. |

## What to copy

There are only two files, and one of them is a capability.

- `src/greeter.rs` — **the part that is actually yours.** `#[zyris::capability(name = .., version =
  ..)]` on a trait generates the descriptor, the `HelloServer<T>` you hand to the runner, and a
  `HelloClient` for consumers. Doc comments become the tool and field descriptions the model reads,
  so write them for the model.
- `src/main.rs` — the wiring, and it is deliberately boring:

  ```rust
  runner
      .kind(NodeKind::Service)
      .request_scopes(["agents:read"])
      .capability(HelloServer(greeter))
      .on_connect(|conn| async move { report_server_capabilities(&conn).await })
      .run()
      .await
  ```

  `Runner::from_env` reads the table above and picks a credential source; `run` owns the dial loop,
  the backoff, the one forced credential rotation on a refusal, `Ctrl-C`, and the exit codes. All of
  that lives in `zyris::runtime` (default feature `runtime`) precisely so it is not something every
  node reimplements slightly differently.

  `report_server_capabilities` in the same file is the **consume** half. A node is not only a tool
  provider: the server announces `attacca_api` on the same websocket, so this process can drive
  agents and sessions while serving `greet`, and `on_connect` is where it picks that client up. The
  client comes from [`zyris-attacca`](../zyris-attacca), which declares that capability with the
  same `#[zyris::capability]` macro the provider side uses — a consumer's half of a capability is an
  ordinary trait, and this one is already written.

  Consuming a capability nobody has published a crate for works the same way, minus the import:
  declare a trait naming the methods you call. Matching is by `(name, version)` and the announced
  tool list is never compared, so one method out of a server's seven, and two fields out of a
  struct's four, still resolve against the real announcement. Declare the slice you call; serde
  ignores the rest.

If you need something the runner does not do — your own supervision tree, a connection per tenant —
`Node::connect` is still the primitive underneath and is not going anywhere. Swap credentials by
implementing `zyris::runtime::Credentials` and using `Runner::new` instead of `from_env`; the three
built-in sources (`StaticToken`, `TokenFile`, `DeviceGrant`) are ordinary impls of that same trait.

## Choosing between enrollment and a static token

Enrollment is right for an interactive install: a machine you are sitting at, or one you SSH'd into,
where there is a person who can approve it. It never asks you to copy a secret between two machines.

A static `znt_` is right for anything provisioned without a human — image-baked nodes, CI, and
**shared service accounts**. Be clear-eyed about the last one: a credential file cannot be protected
from anyone who can `sudo -u` the account that owns it. If several people administer the account
running this node, put a static token in your secret manager instead of enrolling. That is precisely
why static tokens remain supported.

Two things that will bite you if you deviate:

- Pin `schemars` to the same version `zyris` re-exports. The macro expands to
  `::zyris::schemars::schema_for!`, so your types must implement *that* crate's `JsonSchema`.
- Keep `zyris`'s default features on. `Node::connect` lives behind `client`, `Runner` behind
  `runtime`, `zyris::machine_name` behind `hostname`, and the on-disk credential store behind
  `persistence` — all four are default. `enroll` is *not*: a node using only a static token should
  not pay for the device grant, so `zyris-hello` opts into it explicitly.

## Tests

`cargo test -p zyris-hello` exercises the whole announce path over an in-memory duplex — no server
and no database needed. See `tests/greet_roundtrip.rs` for the pattern.
