use rust_decimal::Decimal;
use serde;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::fmt::{Display, Formatter, write};

#[derive(Debug, Deserialize)]
pub struct MarketApiResponse {
    #[serde(rename = "clobTokenIds")]
    pub clob_token_ids: String,
}

#[derive(Debug, Clone)]
pub struct MarketResponse {
    pub first_asset_id: String,
    pub second_asset_id: String,
}

#[derive(Debug, Clone)]
pub struct OrderResponse {
    pub token_id: String,
    pub order_id: String,
}

#[derive(Debug, Clone)]
pub struct HedgeConfig {
    pub asset: Asset,
    pub second_order_id: String,
    pub hedge_asset_id: String,
    pub initial_asset_id: String,
    pub hedge_size: Decimal,
    pub hedge_enter_price: Decimal,
    pub close_size: Decimal,
    pub timestamp: i64,
}

#[derive(Debug, Clone)]
pub struct PreventHoldingConfig {
    pub hedge_config: HedgeConfig,
    pub order_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Asset {
    BTC,
    ETH,
    SOL,
    XRP,
    #[serde(other)]
    Unknown,
}

impl Display for Asset {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let s = match self {
            Asset::BTC => "btc",
            Asset::ETH => "eth",
            Asset::SOL => "sol",
            Asset::XRP => "xrp",
            Asset::Unknown => "UNKNOWN",
        };
        write!(f, "{s}")
    }
}
