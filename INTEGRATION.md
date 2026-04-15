# Integration Guide — okx-liquidity-intel

Two files in the existing CLI repo need one-line edits each.

---

## 1. `cli/src/commands/mod.rs`

Add one line alongside the other `pub mod` declarations:

```rust
// existing lines (context only — do not duplicate)
pub mod defi;
pub mod gateway;
pub mod market;
// ...

// ADD THIS:
pub mod liquidity;
```

---

## 2. `cli/src/main.rs`

### a) Add the variant to the `Commands` enum

```rust
pub enum Commands {
    // existing variants...
    Defi    { command: commands::defi::DefiCommand },
    Upgrade(commands::upgrade::UpgradeArgs),

    // ADD THIS:
    Liquidity { command: commands::liquidity::LiquidityCommand },
}
```

### b) Add the dispatch arm in the `match cli.command` block

```rust
match cli.command {
    // existing arms...
    Commands::Defi    { command } => commands::defi::execute(&ctx, &command).await?,

    // ADD THIS:
    Commands::Liquidity { command } => commands::liquidity::execute(&ctx, &command)?,
}
```

> Note: `liquidity::execute` is synchronous (no `.await`) — all API calls use
> the blocking `client.get()` already used by portfolio and security commands.
> If the project later moves liquidity to async, change the signature in
> `liquidity.rs` to `pub async fn execute(...)` and add `.await?` here.

---

## 3. Verify it compiles

```bash
cd cli
cargo check
```

Expected: zero errors. The only new dependencies are types already in the
workspace (`anyhow`, `clap`, `serde`, `serde_json`) — no `Cargo.toml` changes
needed.

---

## 4. Smoke test (with a live API key)

```bash
# List supported chains
onchainos liquidity chains

# Scan top 5 pools on X Layer
onchainos liquidity scan --chain xlayer --top 5

# Deep-analyze the #1 result (replace ID with actual value from scan output)
onchainos liquidity analyze --pool-id <investment_id> --chain xlayer

# Recommend the best pool to deploy $1,000 of OKB
onchainos liquidity recommend --token OKB --amount 1000 --chain xlayer --risk moderate
```
