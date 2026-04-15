use anyhow::{anyhow, Result};
use clap::{Args, Subcommand};
use serde::Serialize;
use serde_json::{json, Value};

use super::Context;
use crate::output;

// ─── CLI STRUCTURE ────────────────────────────────────────────────────────────

#[derive(Debug, Args)]
pub struct LiquidityArgs {
    #[command(subcommand)]
    pub command: LiquidityCommand,
}

#[derive(Debug, Subcommand)]
pub enum LiquidityCommand {
    /// List blockchains supported by the Liquidity Intelligence Engine
    Chains,
    /// Scan DEX pools, score them, and return ranked opportunities + rejected pools with reasons
    Scan(ScanArgs),
    /// Deep-analysis of a single pool — full scoring breakdown and entry guidance
    Analyze(AnalyzeArgs),
    /// Given a token and USD amount, return a score-weighted capital allocation plan
    Recommend(RecommendArgs),
    /// Return a polling-ready market snapshot with alerts — designed for agent cron loops
    Watch(WatchArgs),
}

#[derive(Debug, Args)]
pub struct ScanArgs {
    /// Blockchain network (xlayer, ethereum, base, bsc, arbitrum, polygon)
    #[arg(long, default_value = "xlayer")]
    pub chain: String,

    /// Filter pools containing this token symbol or contract address
    #[arg(long)]
    pub token: Option<String>,

    /// Filter by DEX platform name, e.g. "uniswap-v3", "uniswap-v4", "okx-dex" (case-insensitive prefix match)
    #[arg(long)]
    pub platform: Option<String>,

    /// Minimum APY threshold to include a pool (percent)
    #[arg(long, default_value = "0.0")]
    pub min_apy: f64,

    /// Minimum TVL threshold to include a pool (USD)
    #[arg(long, default_value = "10000.0")]
    pub min_tvl: f64,

    /// Number of top-ranked pools to return
    #[arg(long, default_value = "10")]
    pub top: usize,

    /// Risk tolerance — shifts scoring weights, not just filters: conservative | moderate | aggressive
    #[arg(long, default_value = "moderate")]
    pub risk: String,

    /// Include bottom-ranked pools with rejection reasoning in the output
    #[arg(long, default_value = "true")]
    pub show_rejected: bool,
}

#[derive(Debug, Args)]
pub struct AnalyzeArgs {
    /// Pool investment ID (from `liquidity scan` output)
    #[arg(long)]
    pub pool_id: String,

    /// Blockchain network
    #[arg(long, default_value = "xlayer")]
    pub chain: String,

    /// Wallet address for personalised position sizing advice (optional)
    #[arg(long)]
    pub address: Option<String>,

    /// Risk tolerance to apply to scoring weights
    #[arg(long, default_value = "moderate")]
    pub risk: String,
}

#[derive(Debug, Args)]
pub struct RecommendArgs {
    /// Token symbol to deploy (e.g. OKB, ETH, USDC)
    #[arg(long)]
    pub token: String,

    /// USD value of the position to deploy
    #[arg(long)]
    pub amount: f64,

    /// Blockchain network
    #[arg(long, default_value = "xlayer")]
    pub chain: String,

    /// Risk tolerance: conservative | moderate | aggressive
    #[arg(long, default_value = "moderate")]
    pub risk: String,
}

#[derive(Debug, Args)]
pub struct WatchArgs {
    /// Blockchain network to monitor
    #[arg(long, default_value = "xlayer")]
    pub chain: String,

    /// Number of top pools to include in the snapshot
    #[arg(long, default_value = "5")]
    pub top: usize,

    /// Minimum score change that triggers an alert (0–100)
    #[arg(long, default_value = "10.0")]
    pub alert_threshold: f64,
}

// ─── OUTPUT TYPES ─────────────────────────────────────────────────────────────

/// Scoring weights — shift with risk profile so the same pool scores differently
/// for a conservative LP vs an aggressive yield farmer.
struct ScoringWeights {
    apy: f64,
    momentum: f64,
    depth: f64,
    il_risk: f64,
    safety: f64,
}

fn scoring_weights(risk: &str) -> ScoringWeights {
    match risk.to_lowercase().as_str() {
        // Conservative: safety, depth, IL protection matter most
        "conservative" => ScoringWeights {
            apy: 0.15,
            momentum: 0.15,
            depth: 0.30,
            il_risk: 0.30,
            safety: 0.10,
        },
        // Aggressive: yield and momentum dominate; willing to accept IL risk
        "aggressive" => ScoringWeights {
            apy: 0.50,
            momentum: 0.30,
            depth: 0.10,
            il_risk: 0.05,
            safety: 0.05,
        },
        // Moderate (default)
        _ => ScoringWeights {
            apy: 0.35,
            momentum: 0.25,
            depth: 0.20,
            il_risk: 0.15,
            safety: 0.05,
        },
    }
}

#[derive(Debug, Serialize, Clone)]
struct ComponentScores {
    apy: f64,
    momentum: f64,
    depth: f64,
    il_risk: f64,
    safety: f64,
}

/// Direction and confidence of the pool's current trend, derived from
/// 24h vs 7d APY and volume signals already available in the API response.
#[derive(Debug, Serialize, Clone)]
struct TrendSignal {
    direction: String,  // "rising" | "stable" | "declining" | "volatile" | "unknown"
    confidence: f64,    // 0.0 – 1.0
    basis: String,      // human-readable explanation shown to the agent
}

#[derive(Debug, Serialize, Clone)]
struct ScoredPool {
    rank: usize,
    pool_id: String,
    pair: String,
    platform: String,
    tvl_usd: f64,
    apy_pct: f64,
    volume_24h_usd: Option<f64>,
    composite_score: f64,
    component_scores: ComponentScores,
    trend: TrendSignal,
    uniswap_v3: Option<UniswapV3Analysis>,
    reasoning: Vec<String>,
    il_warning: Option<String>,
    action: String,
    entry_gas_usd: f64,
}

/// A pool that was evaluated but did not make the top-N cut.
/// Returned alongside top pools so agents understand what was considered and why it was skipped.
#[derive(Debug, Serialize)]
struct RejectedPool {
    pool_id: String,
    pair: String,
    platform: String,
    composite_score: f64,
    primary_rejection_reason: String,
    weakest_component: String,
    weakest_score: f64,
}

/// One leg of a score-weighted capital allocation plan.
#[derive(Debug, Serialize)]
struct AllocationStep {
    rank: usize,
    pool_id: String,
    pair: String,
    platform: String,
    allocation_pct: f64,
    allocation_usd: f64,
    expected_apy_pct: f64,
    projected_annual_yield_usd: f64,
    action: String,
    sizing_rationale: String,
}

/// An alert surfaced by the watch command.
#[derive(Debug, Serialize)]
struct WatchAlert {
    alert_type: String,
    pool: String,
    message: String,
    confidence: f64,
}

// ─── VALUE HELPERS ────────────────────────────────────────────────────────────

fn get_f64(v: &Value, keys: &[&str]) -> Option<f64> {
    for key in keys {
        match v.get(key) {
            Some(Value::Number(n)) => {
                if let Some(f) = n.as_f64() {
                    return Some(f);
                }
            }
            Some(Value::String(s)) => {
                if let Ok(f) = s.parse::<f64>() {
                    return Some(f);
                }
            }
            _ => {}
        }
    }
    None
}

fn get_str<'a>(v: &'a Value, keys: &[&str]) -> Option<&'a str> {
    for key in keys {
        if let Some(s) = v.get(key).and_then(|x| x.as_str()) {
            if !s.is_empty() {
                return Some(s);
            }
        }
    }
    None
}

fn round2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

// ─── DOMAIN CLASSIFIERS ───────────────────────────────────────────────────────

fn is_stablecoin(sym: &str) -> bool {
    matches!(
        sym.to_uppercase().as_str(),
        "USDC" | "USDT" | "DAI" | "BUSD" | "FRAX" | "LUSD"
            | "CRVUSD" | "USDCE" | "USDC.E" | "USDK" | "CUSD"
    )
}

fn is_correlated_pair(a: &str, b: &str) -> bool {
    const GROUPS: &[&[&str]] = &[
        &["ETH", "WETH", "STETH", "RETH", "CBETH", "WSTETH", "EZETH"],
        &["BTC", "WBTC", "CBBTC", "TBTC"],
        &["OKB", "OKT"],
        &["BNB", "WBNB"],
        &["SOL", "WSOL", "MSOL", "JITOSOL"],
    ];
    for group in GROUPS {
        let a_in = group.iter().any(|s| s.eq_ignore_ascii_case(a));
        let b_in = group.iter().any(|s| s.eq_ignore_ascii_case(b));
        if a_in && b_in {
            return true;
        }
    }
    false
}

fn token_safety_score(sym: &str) -> f64 {
    match sym.to_uppercase().as_str() {
        "ETH" | "WETH" | "BTC" | "WBTC" | "CBBTC" => 100.0,
        "OKB" | "BNB" | "WBNB" | "MATIC" | "ARB" | "OP" | "SOL" => 95.0,
        "USDC" | "USDT" | "DAI" | "BUSD" | "USDCE" | "USDC.E" => 98.0,
        "LINK" | "UNI" | "AAVE" | "CRV" | "MKR" | "SNX" | "COMP" => 90.0,
        "STETH" | "RETH" | "WSTETH" | "CBETH" => 92.0,
        _ => 60.0,
    }
}

fn entry_gas_usd(chain: &str) -> f64 {
    match chain.to_lowercase().as_str() {
        "xlayer" => 0.001,
        "polygon" => 0.02,
        "bsc" => 0.05,
        "base" => 0.10,
        "arbitrum" => 0.15,
        "optimism" => 0.20,
        "avalanche" => 0.30,
        "ethereum" => 8.50,
        _ => 0.50,
    }
}

fn action_label(score: f64) -> &'static str {
    if score >= 80.0 {
        "STRONG_BUY"
    } else if score >= 65.0 {
        "BUY"
    } else if score >= 50.0 {
        "WATCH"
    } else if score >= 35.0 {
        "NEUTRAL"
    } else {
        "AVOID"
    }
}

// ─── UNISWAP V3 CONCENTRATED LIQUIDITY ANALYSIS ───────────────────────────────
// Uniswap V3 uses tick-based concentrated liquidity ranges.
// Positions in a narrow range earn more fees per dollar of TVL —
// but get 100% IL if price exits the range.
// We detect Uniswap V3/V4 pools and apply specialised scoring.

fn is_uniswap_platform(platform: &str) -> bool {
    let p = platform.to_lowercase();
    p.contains("uniswap")
}

#[derive(Debug, Serialize, Clone)]
struct UniswapV3Analysis {
    is_uniswap_v3: bool,
    fee_tier_bps: u32,           // 5, 30, 100 = 0.05%, 0.30%, 1.00%
    concentration_bonus: f64,    // extra score for efficient capital use
    range_risk_note: Option<String>,
    recommended_range: String,
}

fn analyse_uniswap_v3(pool: &Value, is_stable: bool, is_corr: bool) -> UniswapV3Analysis {
    let platform = get_str(pool, &["platformName", "platform"]).unwrap_or("");
    let is_v3 = is_uniswap_platform(platform);

    if !is_v3 {
        return UniswapV3Analysis {
            is_uniswap_v3: false,
            fee_tier_bps: 0,
            concentration_bonus: 0.0,
            range_risk_note: None,
            recommended_range: String::new(),
        };
    }

    // Infer fee tier from API data (OKX encodes as decimal: 0.0005, 0.003, 0.01)
    let fee_rate = get_f64(pool, &["feeRate", "fee_rate", "feeTier"]).unwrap_or(0.003);
    let fee_tier_bps = (fee_rate * 10_000.0).round() as u32;

    // Concentrated liquidity earns proportionally more fees per TVL dollar
    // but requires active management. We reward V3 pools with a bonus score.
    let concentration_bonus = match fee_tier_bps {
        5   => 8.0,  // 0.05% — stable pairs: highly efficient, low IL risk
        30  => 5.0,  // 0.30% — standard pairs: good capital efficiency
        100 => 2.0,  // 1.00% — exotic pairs: high fee but wider spreads needed
        _   => 3.0,
    };

    let range_risk_note = if !is_stable && !is_corr {
        Some(format!(
            "Uniswap V3 {:.2}% pool: concentrated liquidity — if price exits your range, \
             the position earns zero fees and is 100% in the depreciating token. \
             Set a ±20–30% range for volatile pairs.",
            fee_rate * 100.0
        ))
    } else if is_corr {
        Some(format!(
            "Uniswap V3 {:.2}% pool: correlated pair — a ±5% tight range captures \
             most fee volume with manageable rebalancing risk.",
            fee_rate * 100.0
        ))
    } else {
        None // stable pair: range risk minimal
    };

    let recommended_range = if is_stable {
        "±0.5% (stable pair — very tight range appropriate)".to_string()
    } else if is_corr {
        "±5–10% (correlated pair — tight range, monitor daily)".to_string()
    } else {
        "±20–30% (volatile pair — wide range reduces out-of-range risk)".to_string()
    };

    UniswapV3Analysis {
        is_uniswap_v3: true,
        fee_tier_bps,
        concentration_bonus,
        range_risk_note,
        recommended_range,
    }
}

// ─── TREND SIGNAL ─────────────────────────────────────────────────────────────

fn derive_trend(apy: f64, apy_7d: f64, vol_24h: f64, vol_7d: f64) -> TrendSignal {
    let vol_7d_daily = vol_7d / 7.0;

    // APY component: compare 24h vs 7d APY
    let apy_ratio = if apy_7d > 0.0 { apy / apy_7d } else { 1.0 };

    // Volume component: compare 24h vs 7d daily average
    let vol_ratio = if vol_7d_daily > 1.0 {
        vol_24h / vol_7d_daily
    } else {
        1.0 // unknown — treat as neutral
    };

    // Classify direction from both signals
    let apy_dir = if apy_ratio > 1.5 {
        2i32 // strongly rising
    } else if apy_ratio > 1.1 {
        1 // rising
    } else if apy_ratio < 0.5 {
        -2 // strongly declining
    } else if apy_ratio < 0.9 {
        -1 // declining
    } else {
        0 // stable
    };

    let vol_dir = if vol_ratio > 1.5 {
        2i32
    } else if vol_ratio > 1.1 {
        1
    } else if vol_ratio < 0.5 {
        -2
    } else if vol_ratio < 0.9 {
        -1
    } else {
        0
    };

    let combined = apy_dir + vol_dir;

    // Detect volatile (signals disagree strongly)
    let is_volatile = (apy_dir - vol_dir).abs() >= 3;

    let (direction, confidence, basis) = if is_volatile {
        (
            "volatile".to_string(),
            0.55,
            format!(
                "APY and volume trends are diverging (APY {:.2}× 7d avg, volume {:.2}× 7d avg) — unusual market dynamics",
                apy_ratio, vol_ratio
            ),
        )
    } else if apy_7d == 0.0 && vol_7d < 1.0 {
        (
            "unknown".to_string(),
            0.0,
            "Insufficient historical data to determine trend direction".to_string(),
        )
    } else {
        match combined {
            3..=4 => (
                "rising".to_string(),
                0.92,
                format!(
                    "Both APY ({:.2}× 7d avg) and volume ({:.2}× 7d avg) are climbing — strong upward trend",
                    apy_ratio, vol_ratio
                ),
            ),
            1..=2 => (
                "rising".to_string(),
                0.72,
                format!(
                    "APY trending {:.2}× above its 7-day average with supportive volume — moderate upward signal",
                    apy_ratio
                ),
            ),
            0 => (
                "stable".to_string(),
                0.80,
                format!(
                    "APY and volume within normal range of 7-day averages (APY {:.2}×, vol {:.2}×) — stable conditions",
                    apy_ratio, vol_ratio
                ),
            ),
            -2..=-1 => (
                "declining".to_string(),
                0.72,
                format!(
                    "APY at {:.2}× its 7-day average with volume down {:.2}× — yield likely to compress",
                    apy_ratio, vol_ratio
                ),
            ),
            _ => (
                "declining".to_string(),
                0.90,
                format!(
                    "Both APY ({:.2}× 7d avg) and volume ({:.2}× 7d avg) are falling — exit signal strengthening",
                    apy_ratio, vol_ratio
                ),
            ),
        }
    };

    TrendSignal {
        direction,
        confidence: round2(confidence),
        basis,
    }
}

// ─── SCORING ENGINE ───────────────────────────────────────────────────────────

fn score_pool(
    pool: &Value,
    all_apys: &[f64],
    all_tvls: &[f64],
    chain: &str,
    weights: &ScoringWeights,
) -> (f64, ComponentScores, TrendSignal, UniswapV3Analysis, Vec<String>, Option<String>) {
    let mut reasoning: Vec<String> = Vec::new();

    let apy = get_f64(pool, &["apy", "apr", "totalApy", "apyYearly"]).unwrap_or(0.0);
    let apy_7d = get_f64(pool, &["apy7d", "apy_7d", "weeklyApy"]).unwrap_or(apy);
    let tvl = get_f64(pool, &["tvl", "tvlUsd", "totalLiquidity"]).unwrap_or(0.0);
    let vol_24h = get_f64(pool, &["volume24h", "volume_24h", "vol24h", "dailyVolume"]).unwrap_or(0.0);
    let vol_7d = get_f64(pool, &["volume7d", "volume_7d", "vol7d", "weeklyVolume"]).unwrap_or(0.0);
    let fee_rate = get_f64(pool, &["feeRate", "fee_rate", "feeTier", "swapFeeRate"]).unwrap_or(0.003);

    let sym_a = pool.get("tokenAInfo")
        .and_then(|t| t.get("symbol")).and_then(|s| s.as_str())
        .or_else(|| get_str(pool, &["tokenASymbol", "token0Symbol", "baseSymbol"]))
        .unwrap_or("TOKEN_A");
    let sym_b = pool.get("tokenBInfo")
        .and_then(|t| t.get("symbol")).and_then(|s| s.as_str())
        .or_else(|| get_str(pool, &["tokenBSymbol", "token1Symbol", "quoteSymbol"]))
        .unwrap_or("TOKEN_B");

    // ── APY Score ─────────────────────────────────────────────────────────────
    let apy_score = if all_apys.len() <= 1 {
        50.0
    } else {
        let rank = all_apys.iter().filter(|&&x| x < apy).count();
        (rank as f64 / (all_apys.len() - 1) as f64) * 100.0
    };

    let top_pct = if all_apys.is_empty() {
        50usize
    } else {
        100 - (all_apys.iter().filter(|&&x| x < apy).count() * 100 / all_apys.len())
    };
    reasoning.push(format!(
        "APY of {:.2}% ranks in the top {}% of all scanned pools on {}",
        apy, top_pct, chain
    ));
    if apy_7d > 0.0 && apy > apy_7d * 2.0 {
        reasoning.push(format!(
            "CAUTION: 24h APY ({:.2}%) is more than 2× the 7-day average ({:.2}%) — spike may not persist",
            apy, apy_7d
        ));
    }

    // ── Momentum Score ────────────────────────────────────────────────────────
    let vol_7d_daily = vol_7d / 7.0;
    let momentum_score = if vol_7d_daily < 1.0 {
        50.0
    } else {
        let ratio = vol_24h / vol_7d_daily;
        if ratio >= 2.0 { 100.0 }
        else if ratio >= 1.5 { 85.0 }
        else if ratio >= 1.2 { 72.0 }
        else if ratio >= 0.8 { 55.0 }
        else if ratio >= 0.5 { 35.0 }
        else { 15.0 }
    };

    if vol_7d_daily > 0.0 {
        let delta = ((vol_24h / vol_7d_daily) - 1.0) * 100.0;
        if delta >= 0.0 {
            reasoning.push(format!(
                "24h volume ${:.0} is {:.0}% above the 7-day daily average — rising fee income expected",
                vol_24h, delta
            ));
        } else {
            reasoning.push(format!(
                "24h volume ${:.0} is {:.0}% below the 7-day daily average — fee income contracting",
                vol_24h, delta.abs()
            ));
        }
    } else {
        reasoning.push("Insufficient historical volume to score momentum — scored as neutral".to_string());
    }

    // ── Liquidity Depth Score ─────────────────────────────────────────────────
    let depth_score = if all_tvls.len() <= 1 {
        50.0
    } else {
        let rank = all_tvls.iter().filter(|&&x| x < tvl).count();
        (rank as f64 / (all_tvls.len() - 1) as f64) * 100.0
    };

    let slippage_est = if tvl > 0.0 { (10_000.0 / tvl) * 50.0 } else { 9999.0 };
    reasoning.push(format!(
        "TVL ${:.0} — estimated slippage on a $10,000 entry: {:.3}%",
        tvl,
        (slippage_est / 100.0).min(99.0)
    ));

    // ── IL Risk Score ─────────────────────────────────────────────────────────
    let is_stable = is_stablecoin(sym_a) || is_stablecoin(sym_b);
    let is_corr = is_correlated_pair(sym_a, sym_b);

    let il_score = if is_stable { 92.0 } else if is_corr { 75.0 } else { 45.0 };

    let il_warning = if !is_stable && !is_corr {
        Some(format!(
            "{}/{} is a volatile uncorrelated pair — impermanent loss may erode yield if prices diverge",
            sym_a, sym_b
        ))
    } else {
        None
    };

    if is_stable {
        reasoning.push(format!("{}/{} includes a stablecoin — IL risk is minimal", sym_a, sym_b));
    } else if is_corr {
        reasoning.push(format!("{}/{} are correlated assets — IL risk is moderate and manageable", sym_a, sym_b));
    } else {
        reasoning.push(format!("{}/{} is a volatile pair — monitor for IL if prices diverge", sym_a, sym_b));
    }

    // ── Safety Score ──────────────────────────────────────────────────────────
    let safety_score = (token_safety_score(sym_a) + token_safety_score(sym_b)) / 2.0;
    reasoning.push(format!(
        "{} + {} token safety: {:.0}/100 — run `security token-scan` before entering",
        sym_a, sym_b, safety_score
    ));

    if fee_rate > 0.0 {
        reasoning.push(format!(
            "Fee tier {:.2}% — fee revenue is the primary driver of the {:.2}% APY",
            fee_rate * 100.0, apy
        ));
    }

    // ── Trend ─────────────────────────────────────────────────────────────────
    let trend = derive_trend(apy, apy_7d, vol_24h, vol_7d);

    // ── Uniswap V3 concentrated liquidity analysis ────────────────────────────
    let univ3 = analyse_uniswap_v3(pool, is_stable, is_corr);
    if univ3.is_uniswap_v3 {
        reasoning.push(format!(
            "Uniswap V3 pool (fee tier {:.2}%) — concentrated liquidity earns \
             higher fees per TVL dollar vs constant-product AMMs. Recommended range: {}",
            univ3.fee_tier_bps as f64 / 100.0,
            univ3.recommended_range
        ));
    }

    // ── Composite (weights vary by risk profile + Uniswap V3 bonus) ──────────
    let base_composite = weights.apy * apy_score
        + weights.momentum * momentum_score
        + weights.depth * depth_score
        + weights.il_risk * il_score
        + weights.safety * safety_score;

    // Uniswap V3 pools get a capital-efficiency bonus, capped at 100
    let composite = round2((base_composite + univ3.concentration_bonus).min(100.0));

    let scores = ComponentScores {
        apy: round2(apy_score),
        momentum: round2(momentum_score),
        depth: round2(depth_score),
        il_risk: round2(il_score),
        safety: round2(safety_score),
    };

    (composite, scores, trend, univ3, reasoning, il_warning)
}

// ─── REJECTION REASONING ──────────────────────────────────────────────────────

fn build_rejected_pool(pool: &ScoredPool) -> RejectedPool {
    // Find the single weakest component to surface as the primary reason
    let components = [
        ("apy", pool.component_scores.apy),
        ("momentum", pool.component_scores.momentum),
        ("depth (TVL)", pool.component_scores.depth),
        ("il_risk", pool.component_scores.il_risk),
        ("safety", pool.component_scores.safety),
    ];

    let (weakest_name, weakest_score) = components
        .iter()
        .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
        .copied()
        .unwrap_or(("composite", pool.composite_score));

    let primary_reason = match weakest_name {
        "apy" => format!(
            "APY score {:.0}/100 — yield ranks in the bottom tier among scanned pools",
            weakest_score
        ),
        "momentum" => format!(
            "Momentum score {:.0}/100 — 24h volume significantly below 7-day average, fee income shrinking",
            weakest_score
        ),
        "depth (TVL)" => format!(
            "Depth score {:.0}/100 — shallow TVL means high slippage risk for meaningful position sizes",
            weakest_score
        ),
        "il_risk" => format!(
            "IL risk score {:.0}/100 — volatile uncorrelated pair, impermanent loss likely to exceed yield at current APY",
            weakest_score
        ),
        "safety" => format!(
            "Safety score {:.0}/100 — one or both tokens are unrecognised; run `security token-scan` before considering",
            weakest_score
        ),
        _ => format!("Composite score {:.1} is below the inclusion threshold", pool.composite_score),
    };

    RejectedPool {
        pool_id: pool.pool_id.clone(),
        pair: pool.pair.clone(),
        platform: pool.platform.clone(),
        composite_score: pool.composite_score,
        primary_rejection_reason: primary_reason,
        weakest_component: weakest_name.to_string(),
        weakest_score: round2(weakest_score),
    }
}

// ─── ALLOCATION PLAN ──────────────────────────────────────────────────────────

fn build_allocation_plan(pools: &[ScoredPool], total_usd: f64, risk: &str) -> Vec<AllocationStep> {
    // How many pools to split across depends on risk tolerance
    let n = match risk.to_lowercase().as_str() {
        "conservative" => 2_usize,
        "aggressive" => 5_usize,
        _ => 3_usize, // moderate
    };
    let n = n.min(pools.len());
    if n == 0 {
        return vec![];
    }

    let selected = &pools[..n];
    let total_score: f64 = selected.iter().map(|p| p.composite_score).sum();

    selected
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let alloc_pct = if total_score > 0.0 {
                round2((p.composite_score / total_score) * 100.0)
            } else {
                round2(100.0 / n as f64)
            };
            let alloc_usd = round2(total_usd * alloc_pct / 100.0);
            let annual_yield = round2(alloc_usd * (p.apy_pct / 100.0));

            let rationale = if i == 0 {
                format!(
                    "Largest allocation ({:.1}%) — highest composite score {:.1} of the {} selected pools",
                    alloc_pct, p.composite_score, n
                )
            } else {
                format!(
                    "{:.1}% allocation — score {:.1} provides diversification while maintaining risk-adjusted yield",
                    alloc_pct, p.composite_score
                )
            };

            AllocationStep {
                rank: i + 1,
                pool_id: p.pool_id.clone(),
                pair: p.pair.clone(),
                platform: p.platform.clone(),
                allocation_pct: alloc_pct,
                allocation_usd: alloc_usd,
                expected_apy_pct: p.apy_pct,
                projected_annual_yield_usd: annual_yield,
                action: "add_liquidity".to_string(),
                sizing_rationale: rationale,
            }
        })
        .collect()
}

// ─── SHARED FETCH + SCORE ─────────────────────────────────────────────────────

/// Returns (top_pools, all_scored_sorted, total_scanned)
/// Keeping `all_scored_sorted` allows callers to build rejected_pools without a second API call.
fn fetch_and_score(
    ctx: &Context,
    chain: &str,
    token: Option<&str>,
    platform_filter: Option<&str>,
    min_apy: f64,
    min_tvl: f64,
    top: usize,
    risk: &str,
) -> Result<(Vec<ScoredPool>, Vec<ScoredPool>, usize)> {
    let chain_id = ctx.chain_index(chain)?;
    let client = ctx.client()?;

    let mut params: Vec<(&str, &str)> = vec![
        ("chainId", &chain_id),
        ("productGroup", "DEX_POOL"),
        ("limit", "50"),
    ];
    let token_owned;
    if let Some(t) = token {
        token_owned = t.to_string();
        params.push(("token", &token_owned));
    }
    // If explicitly requesting Uniswap, pass platform hint to API
    let platform_api_hint: Option<String> = platform_filter
        .filter(|pf| pf.to_lowercase().contains("uniswap"))
        .map(|_| "Uniswap V3".to_string());
    if let Some(ref hint) = platform_api_hint {
        params.push(("platformName", hint));
    }

    let raw: Value = client.get("/api/v5/defi/explore/product/list", &params)?;

    let pools_arr = raw["data"]
        .as_array()
        .ok_or_else(|| anyhow!("API returned no pool data for chain '{}'", chain))?;

    if pools_arr.is_empty() {
        return Err(anyhow!(
            "No DEX pools found on '{}'. Try relaxing --min-apy or --min-tvl.",
            chain
        ));
    }

    let total_scanned = pools_arr.len();
    let weights = scoring_weights(risk);
    let gas = entry_gas_usd(chain);

    let all_apys: Vec<f64> = pools_arr
        .iter()
        .filter_map(|p| get_f64(p, &["apy", "apr", "totalApy", "apyYearly"]))
        .collect();
    let all_tvls: Vec<f64> = pools_arr
        .iter()
        .filter_map(|p| get_f64(p, &["tvl", "tvlUsd", "totalLiquidity"]))
        .collect();

    let mut all_scored: Vec<ScoredPool> = pools_arr
        .iter()
        .filter_map(|p| {
            let apy = get_f64(p, &["apy", "apr", "totalApy", "apyYearly"]).unwrap_or(0.0);
            let tvl = get_f64(p, &["tvl", "tvlUsd", "totalLiquidity"]).unwrap_or(0.0);
            if apy < min_apy || tvl < min_tvl {
                return None;
            }

            let pool_id = get_str(p, &["investmentId", "poolId", "id"])
                .unwrap_or("unknown").to_string();
            let platform = get_str(p, &["platformName", "platform", "dex"])
                .unwrap_or("Unknown DEX").to_string();

            // Client-side platform filter (handles cases API doesn't filter)
            if let Some(pf) = platform_filter {
                if !platform.to_lowercase().contains(&pf.to_lowercase()) {
                    return None;
                }
            }

            let sym_a = p.get("tokenAInfo").and_then(|t| t.get("symbol")).and_then(|s| s.as_str())
                .or_else(|| get_str(p, &["tokenASymbol", "token0Symbol"]))
                .unwrap_or("TOKEN_A");
            let sym_b = p.get("tokenBInfo").and_then(|t| t.get("symbol")).and_then(|s| s.as_str())
                .or_else(|| get_str(p, &["tokenBSymbol", "token1Symbol"]))
                .unwrap_or("TOKEN_B");

            let vol_24h = get_f64(p, &["volume24h", "volume_24h", "vol24h", "dailyVolume"]);
            let (composite, components, trend, univ3, reasoning, il_warning) =
                score_pool(p, &all_apys, &all_tvls, chain, &weights);

            let univ3_opt = if univ3.is_uniswap_v3 { Some(univ3) } else { None };

            Some(ScoredPool {
                rank: 0,
                pool_id,
                pair: format!("{}/{}", sym_a, sym_b),
                platform,
                tvl_usd: round2(tvl),
                apy_pct: round2(apy),
                volume_24h_usd: vol_24h.map(round2),
                composite_score: composite,
                component_scores: components,
                trend,
                uniswap_v3: univ3_opt,
                reasoning,
                il_warning,
                action: action_label(composite).to_string(),
                entry_gas_usd: gas,
            })
        })
        .collect();

    all_scored.sort_by(|a, b| {
        b.composite_score
            .partial_cmp(&a.composite_score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    for (i, p) in all_scored.iter_mut().enumerate() {
        p.rank = i + 1;
    }

    let top_pools: Vec<ScoredPool> = all_scored.iter().take(top).cloned().collect();

    Ok((top_pools, all_scored, total_scanned))
}

// ─── COMMAND HANDLERS ─────────────────────────────────────────────────────────

fn cmd_chains(ctx: &Context) -> Result<()> {
    let client = ctx.client()?;
    let raw: Value = client.get("/api/v5/defi/explore/product/supported-chains", &[])?;
    output::success(raw["data"].clone());
    Ok(())
}

fn cmd_scan(ctx: &Context, args: &ScanArgs) -> Result<()> {
    let (min_apy, min_tvl) = risk_floors(&args.risk, args.min_apy, args.min_tvl);

    let (top_pools, all_scored, total_scanned) = fetch_and_score(
        ctx,
        &args.chain,
        args.token.as_deref(),
        args.platform.as_deref(),
        min_apy,
        min_tvl,
        args.top,
        &args.risk,
    )?;

    let gas = entry_gas_usd(&args.chain);
    let eth_gas = entry_gas_usd("ethereum");

    // Build rejected pools from everything that didn't make top-N
    let rejected: Vec<RejectedPool> = if args.show_rejected {
        all_scored
            .iter()
            .skip(args.top)
            .take(5) // show at most 5 rejected to keep output clean
            .map(build_rejected_pool)
            .collect()
    } else {
        vec![]
    };

    let w = scoring_weights(&args.risk);
    let uniswap_mode = args.platform.as_deref()
        .map(|p| p.to_lowercase().contains("uniswap"))
        .unwrap_or(false);

    output::success(json!({
        "chain": args.chain,
        "risk_profile": args.risk,
        "platform_filter": args.platform,
        "uniswap_mode": uniswap_mode,
        "scoring_note": format!(
            "Weights for '{}' risk — APY×{:.0}% Momentum×{:.0}% Depth×{:.0}% IL×{:.0}% Safety×{:.0}%{}",
            args.risk,
            w.apy * 100.0, w.momentum * 100.0, w.depth * 100.0,
            w.il_risk * 100.0, w.safety * 100.0,
            if uniswap_mode { " + Uniswap V3 concentration bonus applied" } else { "" }
        ),
        "pools_scanned": total_scanned,
        "pools_returned": top_pools.len(),
        "xlayer_advantage": format!(
            "Entry gas on {} costs ~${:.3} vs ~${:.2} on Ethereum — saving ${:.2} per position",
            args.chain, gas, eth_gas, round2(eth_gas - gas)
        ),
        "top_pools": top_pools,
        "rejected_pools": rejected,
    }));
    Ok(())
}

fn cmd_analyze(ctx: &Context, args: &AnalyzeArgs) -> Result<()> {
    let chain_id = ctx.chain_index(&args.chain)?;
    let client = ctx.client()?;
    let weights = scoring_weights(&args.risk);

    let params: Vec<(&str, &str)> = vec![
        ("chainId", &chain_id),
        ("investmentId", &args.pool_id),
    ];

    let raw: Value = client.get("/api/v5/defi/explore/product/detail", &params)?;
    let pool = &raw["data"];
    if pool.is_null() {
        return Err(anyhow!(
            "Pool '{}' not found on {}. Verify the pool_id from `liquidity scan`.",
            args.pool_id, args.chain
        ));
    }

    let apy = get_f64(pool, &["apy", "apr", "totalApy"]).unwrap_or(0.0);
    let tvl = get_f64(pool, &["tvl", "tvlUsd", "totalLiquidity"]).unwrap_or(0.0);

    // Synthetic peer set for relative scoring in a single-pool context
    let peer_apys = vec![apy * 0.4, apy * 0.7, apy, apy * 1.3, apy * 1.8];
    let peer_tvls = vec![tvl * 0.2, tvl * 0.5, tvl, tvl * 2.0, tvl * 5.0];

    let (composite, components, trend, univ3, reasoning, il_warning) =
        score_pool(pool, &peer_apys, &peer_tvls, &args.chain, &weights);

    let sym_a = pool.get("tokenAInfo").and_then(|t| t.get("symbol")).and_then(|s| s.as_str())
        .or_else(|| get_str(pool, &["tokenASymbol", "token0Symbol"]))
        .unwrap_or("TOKEN_A");
    let sym_b = pool.get("tokenBInfo").and_then(|t| t.get("symbol")).and_then(|s| s.as_str())
        .or_else(|| get_str(pool, &["tokenBSymbol", "token1Symbol"]))
        .unwrap_or("TOKEN_B");

    let platform = get_str(pool, &["platformName", "platform"]).unwrap_or("Unknown DEX");
    let gas = entry_gas_usd(&args.chain);
    let univ3_opt = if univ3.is_uniswap_v3 { Some(univ3) } else { None };

    let position_advice = match &args.address {
        Some(addr) => format!(
            "Run `onchainos portfolio all-balances --address {} --chains {}` to verify balance before entering",
            addr, args.chain
        ),
        None => "Pass --address <wallet> for personalised position sizing advice".to_string(),
    };

    output::success(json!({
        "pool_id": args.pool_id,
        "pair": format!("{}/{}", sym_a, sym_b),
        "platform": platform,
        "chain": args.chain,
        "risk_profile": args.risk,
        "apy_24h_pct": get_f64(pool, &["apy", "apr"]),
        "apy_7d_pct": get_f64(pool, &["apy7d", "apy_7d", "weeklyApy"]),
        "tvl_usd": tvl,
        "volume_24h_usd": get_f64(pool, &["volume24h", "volume_24h", "dailyVolume"]),
        "volume_7d_usd": get_f64(pool, &["volume7d", "volume_7d", "weeklyVolume"]),
        "fee_rate": get_f64(pool, &["feeRate", "fee_rate", "feeTier"]),
        "composite_score": composite,
        "action": action_label(composite),
        "component_scores": components,
        "trend": trend,
        "uniswap_v3": univ3_opt,
        "reasoning": reasoning,
        "il_warning": il_warning,
        "entry_gas_usd": gas,
        "position_advice": position_advice,
        "next_steps": [
            format!("Enter pool: `onchainos defi invest --investment-id {} --chain {}`", args.pool_id, args.chain),
            format!("Security: `onchainos security token-scan --chain {} --token-address <{}_contract>`", args.chain, sym_a),
            format!("Security: `onchainos security token-scan --chain {} --token-address <{}_contract>`", args.chain, sym_b),
        ],
    }));
    Ok(())
}

fn cmd_recommend(ctx: &Context, args: &RecommendArgs) -> Result<()> {
    let (min_apy, min_tvl) = risk_floors(&args.risk, 0.0, 0.0);

    let (top_pools, _, total_scanned) = fetch_and_score(
        ctx,
        &args.chain,
        Some(&args.token),
        None, // recommend searches all platforms
        min_apy,
        min_tvl,
        5,
        &args.risk,
    )?;

    if top_pools.is_empty() {
        return Err(anyhow!(
            "No suitable pools found for {} on {} with '{}' risk tolerance. \
             Try --risk aggressive or run `liquidity scan` with no token filter.",
            args.token, args.chain, args.risk
        ));
    }

    let gas = entry_gas_usd(&args.chain);
    let eth_gas = entry_gas_usd("ethereum");
    let allocation_plan = build_allocation_plan(&top_pools, args.amount, &args.risk);

    // Blended yield across all allocation steps
    let total_projected_yield: f64 = allocation_plan
        .iter()
        .map(|s| s.projected_annual_yield_usd)
        .sum();
    let blended_apy = round2((total_projected_yield / args.amount) * 100.0);

    let gas_impact_pct = if args.amount > 0.0 {
        round2((gas * 2.0 * allocation_plan.len() as f64 / args.amount) * 100.0)
    } else {
        0.0
    };

    output::success(json!({
        "token": args.token,
        "deploy_amount_usd": args.amount,
        "chain": args.chain,
        "risk_tolerance": args.risk,
        "pools_evaluated": total_scanned,
        "xlayer_advantage": format!(
            "All-in gas for this {} allocation on {}: ~${:.3} vs ~${:.2} on Ethereum",
            args.risk,
            args.chain,
            round2(gas * 2.0 * allocation_plan.len() as f64),
            round2(eth_gas * 2.0 * allocation_plan.len() as f64),
        ),
        "allocation_plan": allocation_plan,
        "summary": {
            "pools_in_plan": allocation_plan.len(),
            "total_projected_annual_yield_usd": round2(total_projected_yield),
            "blended_apy_pct": blended_apy,
            "total_gas_round_trip_usd": round2(gas * 2.0 * allocation_plan.len() as f64),
            "gas_drag_on_annual_yield_pct": gas_impact_pct,
            "effective_net_apy_pct": round2(blended_apy - gas_impact_pct),
        },
        "entry_steps": top_pools.iter().enumerate().map(|(i, p)| {
            format!(
                "Step {}: `onchainos defi invest --investment-id {} --chain {}`  (${:.0}, {:.1}% APY)",
                i + 1, p.pool_id, args.chain,
                args.amount * (allocation_plan.get(i).map(|a| a.allocation_pct).unwrap_or(0.0) / 100.0),
                p.apy_pct
            )
        }).collect::<Vec<_>>(),
    }));
    Ok(())
}

fn cmd_watch(ctx: &Context, args: &WatchArgs) -> Result<()> {
    let (top_pools, _, total_scanned) = fetch_and_score(
        ctx,
        &args.chain,
        None,
        None, // watch monitors all platforms
        0.0,
        0.0,
        args.top,
        "moderate",
    )?;

    // Generate alerts from trend signals and score characteristics
    let mut alerts: Vec<WatchAlert> = Vec::new();

    for pool in &top_pools {
        // Alert: strong upward trend with high confidence
        if pool.trend.direction == "rising" && pool.trend.confidence >= 0.85 {
            alerts.push(WatchAlert {
                alert_type: "high_momentum".to_string(),
                pool: pool.pair.clone(),
                message: format!(
                    "{} — {}. Entry now captures elevated fee income before it normalises.",
                    pool.pair, pool.trend.basis
                ),
                confidence: pool.trend.confidence,
            });
        }

        // Alert: declining trend in a previously high-scoring pool
        if (pool.trend.direction == "declining") && pool.composite_score >= 65.0 {
            alerts.push(WatchAlert {
                alert_type: "yield_compression_warning".to_string(),
                pool: pool.pair.clone(),
                message: format!(
                    "{} scored {:.1} but trend is declining — consider harvesting yield before APY compresses further.",
                    pool.pair, pool.composite_score
                ),
                confidence: pool.trend.confidence,
            });
        }

        // Alert: APY spike (potential unsustainable yield)
        if let Some(vol_24h) = pool.volume_24h_usd {
            if vol_24h > 0.0 && pool.component_scores.apy >= 90.0 && pool.component_scores.momentum < 50.0 {
                alerts.push(WatchAlert {
                    alert_type: "apy_spike_unconfirmed".to_string(),
                    pool: pool.pair.clone(),
                    message: format!(
                        "{} APY is top-decile but volume momentum is weak — high yield may be a 24h anomaly, not a trend.",
                        pool.pair
                    ),
                    confidence: 0.70,
                });
            }
        }
    }

    // Remove duplicate alerts for the same pool (keep highest confidence)
    alerts.sort_by(|a, b| b.confidence.partial_cmp(&a.confidence).unwrap_or(std::cmp::Ordering::Equal));
    alerts.dedup_by(|a, b| a.pool == b.pool && a.alert_type == b.alert_type);

    // Suggest re-check interval based on market activity
    let avg_momentum: f64 = top_pools.iter().map(|p| p.component_scores.momentum).sum::<f64>()
        / top_pools.len().max(1) as f64;
    let next_check_seconds: u64 = if avg_momentum >= 80.0 { 120 } else if avg_momentum >= 55.0 { 300 } else { 600 };

    output::success(json!({
        "chain": args.chain,
        "pools_scanned": total_scanned,
        "top_pools": top_pools,
        "alerts": alerts,
        "market_pulse": {
            "avg_momentum_score": round2(avg_momentum),
            "activity_level": if avg_momentum >= 75.0 { "high" } else if avg_momentum >= 45.0 { "normal" } else { "low" },
        },
        "next_suggested_check_seconds": next_check_seconds,
        "agent_instruction": format!(
            "Re-run `onchainos liquidity watch --chain {}` in {} seconds or before next capital deployment decision.",
            args.chain, next_check_seconds
        ),
    }));
    Ok(())
}

// ─── RISK FLOORS ─────────────────────────────────────────────────────────────

fn risk_floors(risk: &str, min_apy: f64, min_tvl: f64) -> (f64, f64) {
    let (floor_apy, floor_tvl) = match risk.to_lowercase().as_str() {
        "conservative" => (0.0_f64, 500_000.0_f64),
        "aggressive" => (15.0_f64, 10_000.0_f64),
        _ => (3.0_f64, 50_000.0_f64), // moderate
    };
    (min_apy.max(floor_apy), min_tvl.max(floor_tvl))
}

// ─── ENTRY POINT ──────────────────────────────────────────────────────────────

pub fn execute(ctx: &Context, cmd: &LiquidityCommand) -> Result<()> {
    match cmd {
        LiquidityCommand::Chains => cmd_chains(ctx),
        LiquidityCommand::Scan(a) => cmd_scan(ctx, a),
        LiquidityCommand::Analyze(a) => cmd_analyze(ctx, a),
        LiquidityCommand::Recommend(a) => cmd_recommend(ctx, a),
        LiquidityCommand::Watch(a) => cmd_watch(ctx, a),
    }
}
