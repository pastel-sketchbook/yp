```bash
# Use ASCII art (default)
cargo run

# Or explicitly specify ASCII art
cargo run -- --display-mode ascii

# Use direct image display
cargo run -- --display-mode direct
```

You can also use the short form:
```bash
cargo run -- -d ascii
cargo run -- -d direct
```