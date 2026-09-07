# COSMIC Text Editor
Text editor for the COSMIC desktop

![Screenshot](res/screenshots/screenshot-1.png)

Currently an incomplete **pre-alpha**, this project is a work in progress - issues are expected.

## Testing
You can test by installing a current version of Rust and then building with `cargo`.

```SHELL
git clone https://github.com/pop-os/cosmic-edit
cd cosmic-edit
cargo build
```

You can get more detailed errors by using the `RUST_LOG` environment variables, that you can invoke for just that one command like this: `RUST_LOG=debug cargo run`. This will give you more detail about the application state. You can go even further with `RUST_LOG=trace cargo run`, that shows all logging details about the application.

## Clippy Lints
PRs are welcome, as it builds a better product for everyone. It is recommended that you check your code with Clippy Lints turned on. You can find more about [Configuring Clippy](https://doc.rust-lang.org/nightly/clippy/configuration.html) here.

## Claw OS MCP service

The App Host starts `/usr/bin/cosmic-edit` with `COS_MCP_SERVER=1`.
`apps/cosmic-edit/app.json` owns the MCP tool descriptions, arguments,
defaults, and capability needs; Rust binds only the handlers.

All seven App tools use controlled primitives, never another App:
filesystem reads/writes/replacements use `cos_runtime::filesystem`, opening
uses the fixed `com.clawos.Edit` desktop target, and AI uses SDK
`ai::chat` as `cosmic-edit` with `external-content` origin. Summaries do not
write memory. Replacement proposals do not modify the file.

Text files are limited to 1,000,000 bytes; oversize and invalid UTF-8 reads
fail rather than returning partial content. Writes are atomic, require an
existing parent directory, and record task-owned inverse snapshots.
`edit.replace_range` requires exactly one match and never saves a partial
read. File opening requires exact `fs.read` alongside the fixed launch scope.
These App handlers are distinct from the trusted-human interactive UI bridges.

From the repository root, run the handler/AI bridge test without GUI backends:

```sh
cargo test --manifest-path desktop/edit/Cargo.toml --no-default-features mcp::tests:: -- --test-threads=1
```
