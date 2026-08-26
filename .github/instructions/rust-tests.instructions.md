---
applyTo: "**/test/unit/**/*.rs,**/tests/**/*.rs"
---

# Rust Tests

- Read the matching production source and its callers before changing tests.
- `test/unit/` files are included by their production module so they retain
  private access; do not turn private implementation into public API for tests.
- Mirror production source paths and keep test names/filters stable.
- Unit tests cover internal invariants; crate-level `tests/` files cover public
  APIs and process/integration behavior.
- Do not load unrelated test directories during initial repository discovery.
- Run the narrowest test filter first, then the affected crate suite.
