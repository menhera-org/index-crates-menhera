# index.crates.menhera.org

A Cargo sparse-index proxy for filtering out too-new
versions of Rust crates. Written for Fastly Compute.

It should:

- serve sparse-index repos at `/[0-30]d/` (every integer from `/0d/` to `/30d/`), with e.g. `/3d/config.toml`, `/12d/config.toml`, etc. just forwarded to `https://index.crates.io/config.toml` (which should be cached by Fastly).
- each repo filters sparse-index entries with minimum publish age requirements. `/3d/` endpoint filters any versions newer than 3-day-old. `/0d/` applies no filter (pure pass-through).
- 404s for everything else.

## License

SPDX-License-Identifier: Apache-2.0 OR MPL-2.0

