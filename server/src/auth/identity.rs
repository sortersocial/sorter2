//! Trust-weight calculation from linked OAuth providers.

/// Base weight before any OAuth links.
pub const BASE_TRUST_WEIGHT: f64 = 1.0;

/// Increment per linked provider (frozen at vote cast time).
pub const TRUST_WEIGHT_PER_LINK: f64 = 0.5;

pub fn trust_weight_for_link_count(link_count: usize) -> f64 {
    BASE_TRUST_WEIGHT + TRUST_WEIGHT_PER_LINK * link_count as f64
}

pub fn trust_weight_after_link(current: f64) -> f64 {
    current + TRUST_WEIGHT_PER_LINK
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trust_weight_scales_with_links() {
        assert_eq!(trust_weight_for_link_count(0), 1.0);
        assert_eq!(trust_weight_for_link_count(1), 1.5);
        assert_eq!(trust_weight_for_link_count(2), 2.0);
    }
}
