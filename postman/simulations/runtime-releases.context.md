# Runtime release verification

The public GitHub release contract and examples were captured on 2026-10-25.
Only the fields consumed by Ember are retained. The Postman CLI was signed out,
so an owner-workspace mock search was skipped. Nothing is pushed or deployed.

Start the local simulator from the repository root:

```sh
postman simulation run postman/simulations/runtime-releases.sim.yaml --port 4900 --output ndjson
```

Wait for its `listening` event. In another terminal, use that event's bound port:

```sh
EMBER_RELEASE_TEST_URL=http://127.0.0.1:4900 cargo test -p ember-runtime simulated_macos_releases -- --ignored
```

The test runs the production HTTP request and release parser for all three macOS
sources, and checks that a 429 is surfaced as an error. The successful member's
request events must show the three `/repos/.../releases?per_page=1` requests.
Injected failures do not appear in the mock handler log. Unmodeled routes return
501 so they cannot masquerade as GitHub's 404 response.

This checks HTTP wiring and asset selection. It does not establish archive
integrity, GPU compatibility, a game's DRM, or behavior of installed runtimes.
Stop with Ctrl+C. No user configuration, accounts, or runtime installs are changed.
