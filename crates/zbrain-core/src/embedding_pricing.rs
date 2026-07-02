//! #111: Embedding pricing table
//!
//! 模型 → $/MTok 查找表（OpenAI/Voyage/ZeroEntropy 等）

/// 嵌入模型定价信息
#[derive(Debug, Clone)]
pub struct EmbeddingPricing {
    pub model: &'static str,
    pub provider: &'static str,
    pub price_per_mtok_usd: f64,  // USD per million tokens
    pub dimensions: usize,
}

/// 定价表（按模型名称查找）
static PRICING_TABLE: &[EmbeddingPricing] = &[
    // OpenAI
    EmbeddingPricing {
        model: "text-embedding-3-small",
        provider: "openai",
        price_per_mtok_usd: 0.02,
        dimensions: 1536,
    },
    EmbeddingPricing {
        model: "text-embedding-3-large",
        provider: "openai",
        price_per_mtok_usd: 0.13,
        dimensions: 3072,
    },
    EmbeddingPricing {
        model: "text-embedding-ada-002",
        provider: "openai",
        price_per_mtok_usd: 0.10,
        dimensions: 1536,
    },
    // Voyage
    EmbeddingPricing {
        model: "voyage-2",
        provider: "voyage",
        price_per_mtok_usd: 0.10,
        dimensions: 1024,
    },
    EmbeddingPricing {
        model: "voyage-large-2",
        provider: "voyage",
        price_per_mtok_usd: 0.12,
        dimensions: 1536,
    },
    // ZeroEntropy
    EmbeddingPricing {
        model: "zeroentropyai:zembed-1",
        provider: "zeroentropy",
        price_per_mtok_usd: 0.05,
        dimensions: 1280,
    },
];

/// 根据模型名称查找定价信息
pub fn lookup_pricing(model: &str) -> Option<&'static EmbeddingPricing> {
    PRICING_TABLE
        .iter()
        .find(|p| p.model == model)
}

/// 计算嵌入成本（USD）
pub fn estimate_cost_usd(model: &str, num_chunks: usize, avg_tokens_per_chunk: usize) -> f64 {
    if let Some(pricing) = lookup_pricing(model) {
        let total_tokens = num_chunks * avg_tokens_per_chunk;
        let mtok = total_tokens as f64 / 1_000_000.0;
        mtok * pricing.price_per_mtok_usd
    } else {
        0.0  // 未知模型，假设免费
    }
}

// --- 测试 ---

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_pricing_known_model() {
        let result = lookup_pricing("text-embedding-3-small");
        assert!(result.is_some());
        let pricing = result.unwrap();
        assert_eq!(pricing.provider, "openai");
        assert_eq!(pricing.dimensions, 1536);
        assert_eq!(pricing.price_per_mtok_usd, 0.02);
    }

    #[test]
    fn lookup_pricing_unknown_model() {
        let result = lookup_pricing("unknown-model-x");
        assert!(result.is_none());
    }

    #[test]
    fn lookup_pricing_zeroentropy_model() {
        let result = lookup_pricing("zeroentropyai:zembed-1");
        assert!(result.is_some());
        let pricing = result.unwrap();
        assert_eq!(pricing.provider, "zeroentropy");
        assert_eq!(pricing.dimensions, 1280);
    }

    #[test]
    fn estimate_cost_usd_known_model() {
        // 1000 chunks × 500 tokens = 500K tokens = 0.5 MTok
        // text-embedding-3-small: $0.02/MTok
        // Expected: 0.5 × 0.02 = $0.01
        let cost = estimate_cost_usd("text-embedding-3-small", 1000, 500);
        assert!((cost - 0.01).abs() < 0.0001);
    }

    #[test]
    fn estimate_cost_usd_unknown_model() {
        let cost = estimate_cost_usd("unknown-model", 1000, 500);
        assert_eq!(cost, 0.0);
    }

    #[test]
    fn estimate_cost_usd_zeroentropy() {
        // 2000 chunks × 800 tokens = 1.6M tokens = 1.6 MTok
        // zeroentropyai:zembed-1: $0.05/MTok
        // Expected: 1.6 × 0.05 = $0.08
        let cost = estimate_cost_usd("zeroentropyai:zembed-1", 2000, 800);
        assert!((cost - 0.08).abs() < 0.0001);
    }

    #[test]
    fn pricing_table_covers_all_models_in_config_default() {
        // 确保默认模型在定价表中
        let default_model = "zeroentropyai:zembed-1";
        assert!(lookup_pricing(default_model).is_some());
    }
}
