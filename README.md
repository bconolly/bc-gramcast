# Slotcast

Self-hosted scheduler that auto-publishes the same video as an Instagram Reel and a YouTube Short on a weekly slot pattern, with platform analytics and Payhip sales in one place. Runs on Cloudflare's free tiers.

Work in progress. Slice 1 (upload and queue) is under way.

## Develop
```
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run --workspace
```

`spike/cpu` measures how long one R2 presign takes in V8 WebAssembly, to check the Workers free plan's 10 ms CPU limit.
