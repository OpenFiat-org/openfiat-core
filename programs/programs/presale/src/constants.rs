use anchor_lang::prelude::*;

/// PDA seed for the singleton `SaleConfig` account (OFS-4200 §3).
#[constant]
pub const SALE_CONFIG_SEED: &[u8] = b"sale_config";
