# okx-liquidity-intel — Liquidity Intelligence Engine (LIE)

> **The decision layer that powers autonomous DeFi agents on X Layer.**

Not a dashboard. Not a data feed. A **reasoning engine** — every response tells an agent not just *what* the pool market looks like, but *why* a specific pool was selected, *why* competitors were rejected, the *direction* of the current trend, and *exactly what to do next* with a given amount of capital.

---

## Project Introduction

`okx-liquidity-intel` is an OnchainOS skill that scans DEX liquidity pools across X Layer (and other EVM chains), scores every pool using a transparent 5-component weighted algorithm, and returns structured, machine-readable intelligence that any AI agent can act on without hallucination.

**The core problem it solves:** AI agents currently lack a standard way to evaluate DEX pools. They either query raw API data (no signal) or rely on vague heuristics (no trust). This skill provides a **consistent, explainable, auditable scoring layer** — the missing primitive between "I have capital" and "I know where to deploy it."

**Why X Layer wins:** X Layer's near-zero gas (~$0.001 per transaction vs ~$8.50 on Ethereum) means the skill's net yield calculations are fundamentally different from Ethereum-first tools. Every output includes a `xlayer_advantage` field that quantifies the gas saving in USD so agents and users can see the real difference.

---

## Architecture Overview

```
┌─────────────────────────────────────────────────────────────────┐
│                        AI Agent / User                          │
└────────────────────────────┬────────────────────────────────────┘
                             │ natural language intent
                             ▼
┌─────────────────────────────────────────────────────────────────┐
│                    OnchainOS Skill Router                       │
│              (matches intent → okx-liquidity-intel)             │
└────────────────────────────┬────────────────────────────────────┘
                             │
          ┌──────────────────┼──────────────────┐
          ▼                  ▼                  ▼
   liquidity scan     liquidity recommend   liquidity watch
   liquidity analyze  liquidity chains      (cron/polling)
          │                  │                  │
          └──────────────────┴──────────────────┘
                             │
                             ▼
┌─────────────────────────────────────────────────────────────────┐
│              Liquidity Intelligence Engine (Rust)               │
│                                                                 │
│  fetch_and_score()                                              │
│  ├── OKX DeFi API  (/api/v5/defi/explore/product/list)         │
│  │   └── DEX_POOL products, platform filter, token filter      │
│  │                                                              │
│  └── Scoring Engine                                             │
│      ├── APY Score        (percentile rank, 35% weight)        │
│      ├── Momentum Score   (24h vs 7d volume, 25% weight)       │
│      ├── Depth Score      (TVL percentile → slippage, 20%)     │
│      ├── IL Risk Score    (stable/corr/volatile, 15% weight)   │
│      ├── Safety Score     (token tier, 5% weight)              │
│      ├── Trend Signal     (direction + confidence + basis)      │
│      └── Uniswap V3 Bonus (concentration efficiency, +2–8 pts) │
│                                                                 │
│  Outputs: ScoredPool[] + RejectedPool[] + AllocationPlan[]     │
└──────────────────────────────┬──────────────────────────────────┘
                               │ composes with
         ┌─────────────────────┼─────────────────────┐
         ▼                     ▼                     ▼
  okx-security          okx-defi-invest       okx-wallet-portfolio
  (token-scan)          (execute entry)       (balance check)
         │
         ▼
  okx-agentic-wallet   okx-onchain-gateway   okx-dex-market
  (identity/signing)   (broadcast tx)        (price reference)
```

### Key design decisions

- **No persistent state** — every scan is a live API call. No stale cache risk.
- **Risk-weighted scoring** — `--risk conservative/moderate/aggressive` shifts the 5 scoring weights, not just filter thresholds. A conservative agent and an aggressive agent will rank the same pool differently — by design.
- **Rejected pools included** — the skill always returns the pools it *didn't* pick, with the primary rejection reason. This gives agents comparative context, not just a winner list.
- **Uniswap V3 aware** — pools on Uniswap V3/V4 receive a concentration efficiency bonus (+2 to +8 composite points) and a specialised `recommended_range` output for tick-based position management.

---

## Onchain Identity — Agentic Wallet

This project uses the `okx-agentic-wallet` skill to establish its onchain identity on X Layer.

**Setup (run once):**
```bash
# 1. Authenticate
onchainos wallet login <your-email>
onchainos wallet verify <verification-code>

# 2. Get your X Layer address (chain ID 196)
onchainos wallet addresses --chain 196

# 3. Verify the wallet is live on X Layer
onchainos portfolio total-value --address <xlayer-address> --chains xlayer
```

**Deployment Address (X Layer, chain ID 196):**
```
0x_YOUR_XLAYER_WALLET_ADDRESS_HERE
```
> Replace this with the address returned by `onchainos wallet addresses --chain 196` after running setup.

**Role of the wallet in this project:**
- Acts as the agent's identity when calling `okx-defi-invest` to enter pools
- Signs transactions via TEE (private key never exposed)
- Provides balance verification before the skill recommends a position size
- All X Layer transactions are broadcast through `okx-onchain-gateway`

---

## OnchainOS Skill Usage

This skill **composes with 6 existing OnchainOS skills** in its recommended agent workflow:

| Skill | How it's used |
|---|---|
| `okx-agentic-wallet` | Agent identity, TEE-based transaction signing, wallet address discovery |
| `okx-security` | `token-scan` on both pool tokens before any entry — blocks honeypots |
| `okx-defi-invest` | Executes the actual LP deposit after this skill approves the pool |
| `okx-wallet-portfolio` | Balance check before position sizing in `liquidity recommend` |
| `okx-dex-market` | Price reference for slippage validation on large positions |
| `okx-onchain-gateway` | Broadcasts signed transactions on X Layer |

---

## Uniswap Integration

`okx-liquidity-intel` provides first-class Uniswap V3 / V4 support:

**1. Dedicated scan mode:**
```bash
onchainos liquidity scan --chain xlayer --platform uniswap-v3 --top 5
```
Returns only Uniswap pools, ranked by the LIE scoring algorithm.

**2. Concentrated liquidity scoring:**
Every Uniswap V3/V4 pool receives a `uniswap_v3` block in the response:
```json
"uniswap_v3": {
  "is_uniswap_v3": true,
  "fee_tier_bps": 30,
  "concentration_bonus": 5.0,
  "recommended_range": "±5–10% (correlated pair — tight range, monitor daily)",
  "range_risk_note": "Uniswap V3 0.30% pool: if price exits your range, position earns zero fees..."
}
```

**3. Fee tier intelligence:**
- `0.05%` pools (stable pairs) → +8 concentration bonus
- `0.30%` pools (standard) → +5 concentration bonus
- `1.00%` pools (exotic) → +2 concentration bonus

This directly targets the **Best Uniswap Integration** special prize.

---

## Working Mechanics

### Command reference

| Command | What it does |
|---|---|
| `onchainos liquidity chains` | List supported chains |
| `onchainos liquidity scan` | Score + rank all pools; returns top pools + rejected pools |
| `onchainos liquidity analyze` | Deep single-pool breakdown with entry instructions |
| `onchainos liquidity recommend` | Score-weighted capital allocation plan across 2–5 pools |
| `onchainos liquidity watch` | Polling-ready snapshot with typed alerts for agent cron loops |

### The scoring formula

```
composite_score = Σ(weight_i × component_score_i) + uniswap_v3_bonus

where weights shift by --risk:
  conservative: APY×15%  Momentum×15%  Depth×30%  IL×30%  Safety×10%
  moderate:     APY×35%  Momentum×25%  Depth×20%  IL×15%  Safety×5%
  aggressive:   APY×50%  Momentum×30%  Depth×10%  IL×5%   Safety×5%
```

### Example: scan output (condensed)
```bash
onchainos liquidity scan --chain xlayer --platform uniswap-v3 --risk moderate --top 3
```
```json
{
  "chain": "xlayer",
  "uniswap_mode": true,
  "xlayer_advantage": "Entry gas on xlayer costs ~$0.001 vs ~$8.50 on Ethereum — saving $8.499 per position",
  "top_pools": [
    {
      "rank": 1,
      "pair": "OKB/USDC",
      "composite_score": 92.3,
      "action": "STRONG_BUY",
      "trend": { "direction": "rising", "confidence": 0.92, "basis": "Both APY and volume 1.4× above 7d avg" },
      "uniswap_v3": { "fee_tier_bps": 30, "concentration_bonus": 5.0, "recommended_range": "±0.5%" },
      "reasoning": [
        "APY of 34.5% ranks in the top 8% of scanned pools on xlayer",
        "24h volume 42% above 7-day average — rising fee income",
        "TVL $2.45M — slippage on $10,000 entry: 0.002%",
        "OKB/USDC includes a stablecoin — IL risk minimal",
        "Uniswap V3 pool (0.30%) — concentrated liquidity. Recommended range: ±0.5%"
      ]
    }
  ],
  "rejected_pools": [
    {
      "pair": "MEME/ETH",
      "composite_score": 28.1,
      "primary_rejection_reason": "IL risk score 45/100 — volatile uncorrelated pair, impermanent loss likely to exceed yield",
      "weakest_component": "il_risk"
    }
  ]
}
```

### Example: capital allocation plan
```bash
onchainos liquidity recommend --token OKB --amount 5000 --chain xlayer --risk moderate
```
```json
{
  "allocation_plan": [
    { "pair": "OKB/USDC", "allocation_pct": 54.2, "allocation_usd": 2710, "expected_apy_pct": 34.5, "projected_annual_yield_usd": 935 },
    { "pair": "OKB/WETH", "allocation_pct": 28.6, "allocation_usd": 1430, "expected_apy_pct": 28.1, "projected_annual_yield_usd": 402 },
    { "pair": "OKB/DAI",  "allocation_pct": 17.2, "allocation_usd":  860, "expected_apy_pct": 21.3, "projected_annual_yield_usd": 183 }
  ],
  "summary": {
    "blended_apy_pct": 30.4,
    "total_projected_annual_yield_usd": 1520,
    "total_gas_round_trip_usd": 0.006,
    "effective_net_apy_pct": 30.4
  }
}
```

---

## X Layer Ecosystem Positioning

`okx-liquidity-intel` is built **for X Layer first**:

1. **Default chain is `xlayer`** — every command targets X Layer unless explicitly overridden.
2. **Gas math is central** — the `xlayer_advantage` field in every response quantifies the gas saving in USD. For a $500 position, X Layer saves $8.499 in entry gas vs Ethereum — that's a 1.7% immediate yield advantage before APY even factors in.
3. **Native OKB support** — OKB pool pairs are explicitly tier-1 in the safety classifier (score: 95/100), giving OKX's native token fair representation in rankings.
4. **X Layer chain ID 196** is hardcoded in the gas table and wallet setup, ensuring all agent workflows default to the correct network.
5. **Near-zero gas enables the watch loop** — polling `liquidity watch` every 2–5 minutes costs ~$0.002/day on X Layer. On Ethereum this would be economically impractical.

---

## Team Members

| Name | Role |
|---|---|
| [Your Name] | Lead developer — skill architecture, scoring algorithm, Rust implementation |

---

## Bonus

### Demo Video
> Record a 1–3 minute demo showing:
> 1. `onchainos liquidity scan --chain xlayer --platform uniswap-v3 --top 5`
> 2. `onchainos liquidity recommend --token OKB --amount 5000 --chain xlayer`
> 3. `onchainos liquidity watch --chain xlayer` (live alerts)
>
> Upload to YouTube and paste the link here: **[VIDEO LINK]**

### X Post
> Post a project intro with #onchainos @XLayerOfficial and paste the link here: **[POST LINK]**

---

## Scoring Criteria Self-Assessment

| Criterion (25% each) | Evidence |
|---|---|
| **OnchainOS/Uniswap integration** | Composes with 6 OnchainOS skills; Uniswap V3 `--platform uniswap-v3` filter; concentration bonus scoring; tick range recommendations |
| **X Layer ecosystem** | Default chain xlayer; `xlayer_advantage` USD field in every response; OKB tier-1 classification; watch loop economically viable only on X Layer |
| **AI interactive experience** | 6-field reasoning chain per pool; typed trend signals with confidence; rejected pools with plain-English reasons; watch alerts with `agent_instruction`; allocation plan with `sizing_rationale` per step |
| **Product completeness** | 5 CLI commands; ~600-line Rust implementation; comprehensive SKILL.md; agentic wallet integration; Uniswap V3 analysis; this README |

---

## Getting Started

```bash
# Install OnchainOS CLI
npm install -g @onchainos/cli

# Set up agentic wallet (your project's onchain identity)
onchainos wallet login <email>
onchainos wallet verify <code>
onchainos wallet addresses --chain 196   # save this address

# Run your first scan
onchainos liquidity scan --chain xlayer --top 5

# Target Uniswap V3 specifically
onchainos liquidity scan --chain xlayer --platform uniswap-v3 --top 5

# Get a capital allocation plan
onchainos liquidity recommend --token OKB --amount 1000 --chain xlayer

# Start the monitoring loop
onchainos liquidity watch --chain xlayer
```

---

## File Structure

```
okx-liquidity-intel/
├── README.md                          ← this file
├── INTEGRATION.md                     ← how to wire into mod.rs and main.rs
├── skills/
│   └── okx-liquidity-intel/
│       └── SKILL.md                   ← agent rules, command spec, output schema
└── cli/
    └── src/
        └── commands/
            └── liquidity.rs           ← full Rust implementation (~650 lines)
```
