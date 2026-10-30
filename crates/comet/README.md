# comet

An open-source GOG Galaxy communication service by Paweł Lidwin, vendored from
<https://github.com/imLinguin/comet> at
`46c91529a07ab4d4ab0397429dc07d4f2f66431a` (comet 0.4.1, reviewed). It is Apache-2.0
(`LICENSE-APACHE`). The protocol definitions in `proto/` are from
<https://github.com/Yepoleb/gog_protocols> at
`923e8a0008603365d29002db611559190da5bf99` (CC0, `proto/LICENSE`).

Irori hosts this as a library through `crates/gog-galaxy`. Upstream is a program a
user starts on purpose; here it listens on a port every local program can reach, and
a panic takes down the whole app because release builds abort on panic. That is why
this fork exists. Keep it small, so that upstream changes still apply.

## What differs from upstream

Removed:

- The command-line program (`main.rs`, `import_parsers`) and what only it used.
- `external/`. Upstream dropped its pinned certificates too; GOG's hosts chain to public
  roots.

Who gets served (`api/handlers.rs`, `handlers/context.rs`):

- Only clients the host names in `AllowedClients` may sign in.
- Galaxy's own client id never signs in. It is public, and its token is good for the
  whole account.
- `ACCESS_TOKEN_REQUEST` and the rest of the overlay's calls (sorts 3 and 7) are refused
  on the port. The real overlay only uses the pipe
  `Galaxy-{pid}-CommunicationService-Overlay`.
- `START_GAME_SESSION` no longer opens that pipe. Irori neither downloads nor injects
  the overlay, and the pid in the request would let any client that signed in open it.
  Bringing the overlay to Irori needs a way to tell the overlay from other programs
  first.
- Topic messages (chat, presence) are forwarded only after sign-in.

Nothing a client sends may panic:

- Client and user ids must be decimal numbers before they reach a path or an address.
- Calls that need a client wait for sign-in, and a client counts as signed in only
  after its database is open. Upstream handles messages concurrently, so a call could
  otherwise reach a database that is not open yet.
- Message types, ids and sort methods out of range are errors, messages over 16 MiB are
  refused, and database failures end the sync or the request instead of the process.
- `NotificationPusherClient::new` returns `None` if the service stops before it could
  connect, which upstream turned into a panic whenever a game exited while offline.

Components (`api/gog/components.rs`):

- Every file must match the manifest's `sha256`, which is the hash of the unpacked file,
  not of the zip, and is written in one step.
- Manifest paths and symlinks must stay inside the component.
- Files from before this check are downloaded once more.

Dependencies:

- Irori's reqwest 0.12 instead of 0.13, whose default TLS backend would add aws-lc-rs
  next to ring.
- The notification socket uses its own ring-backed rustls config with the webpki roots,
  so it cannot hit rustls's panic when no single provider can be picked.
- sqlx without its default features, and without SQLite extension loading.
