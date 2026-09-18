# letmerelease

Publish every crate of a cargo workspace in dependency order.

It is a thin wrapper over `cargo publish`. Crates with `publish = false` are skipped, and workspaces with more than 20 crates get a pause every few crates so crates.io rate limits are not hit.

## Install

```sh
cargo install letmerelease
```

## Usage

```sh
letmerelease              # publish the workspace
letmerelease --dry-run    # print the plan and run cargo publish --dry-run
letmerelease --registry my-registry  # publish to a custom Cargo registry
letmerelease -- --no-verify          # forward arguments to cargo publish
```

| Flag | Default | Meaning |
|------|---------|---------|
| `--dry-run` | off | Print the order and verify without uploading |
| `--batch` | 5 | Crates to publish before pausing |
| `--wait` | 180 | Seconds to pause between batches |
| `--large` | 20 | Pausing only kicks in above this many crates |
| `--registry` | crates.io | Registry passed to `cargo publish` |

Authentication is left to cargo, so use `cargo login` or `CARGO_REGISTRY_TOKEN`.

## GitHub Action

```yaml
- uses: marc2332/letmerelease@v0.1.0
  with:
    token: ${{ secrets.CARGO_REGISTRY_TOKEN }}
    # dry-run: true
    # args: --batch 10 --wait 120
```

## License

MIT
