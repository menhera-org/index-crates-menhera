# index.crates.menhera.org

A Cargo sparse-index proxy for filtering out too-new
versions of Rust crates. Written for Fastly Compute.

It should:

- serve sparse-index repos at `/[1-30]d/` (every integer from `/1d/` to `/30d/`), with e.g. `/3d/config.toml`, `/12d/config.toml`, etc. just forwarded to `https://index.crates.io/config.toml` (which should be cached by Fastly).
- each of 30 repos filters sparse-index entries with minimum publish age requirements. `/3d/` endpoint filters any versions newer than 3-day-old.
- 404s for everything else.
