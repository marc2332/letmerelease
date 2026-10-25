# letmerelease

Publish every crate of a cargo workspace in dependency order.

It is a thin wrapper over `cargo publish`. Crates with `publish = false` are skipped. Publishing starts with a burst of 30 crates, then waits 4 minutes before each subsequent batch of 3 crates. There is no pause after the final crate.

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
| `--burst` | 30 | Crates to publish before the first pause |
| `--batch` | 3 | Crates per batch after the initial burst |
| `--wait` | 240 | Seconds to pause after the burst and between batches |
| `--registry` | crates.io | Registry passed to `cargo publish` |
| `--ignore-published` | off | Include crates whose versions are already published |

Authentication is left to cargo, so use `cargo login` or `CARGO_REGISTRY_TOKEN`.

## GitHub Action

```yaml
- uses: marc2332/letmerelease@v0.2.0
  with:
    token: ${{ secrets.CARGO_REGISTRY_TOKEN }}
    # dry-run: true
    # args: --burst 30 --batch 3 --wait 240
```

## License

MIT
