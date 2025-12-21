use crate::dto::{Asset, OrderResponse};
use crate::{MarketApiResponse, MarketResponse};
use alloy::signers::k256::ecdsa::SigningKey;
use alloy::signers::k256::ecdsa::signature::SignerMut;
use alloy::signers::local::LocalSigner;
use chrono::{Local, TimeZone, Timelike};
use polymarket_client_sdk::auth::Normal;
use polymarket_client_sdk::clob::Client;
use polymarket_client_sdk::clob::state::Authenticated;
use polymarket_client_sdk::types::{
    Amount, OrderType, PostOrderResponse, PriceRequestBuilder, PriceResponse, Side,
};
use reqwest::Client as http_client;
use rust_decimal::{Decimal, RoundingStrategy};
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::time::sleep;

pub fn allow_stop_loss(market_timestamp: i64, grace_seconds: i64) -> bool {
    // текущее время в unix timestamp (UTC)
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time went backwards")
        .as_secs() as i64;

    // если мы раньше старта рынка — стоп запрещён
    if now < market_timestamp {
        return false;
    }

    let seconds_since_start = now - market_timestamp;

    // если рынок уже закончился (15 минут = 900 секунд)
    if seconds_since_start >= 900 {
        return false;
    }

    // разрешаем стоп только после grace_seconds
    seconds_since_start >= grace_seconds
}

pub fn floor_dp(value: Decimal, dp: u32) -> Decimal {
    value.round_dp_with_strategy(dp, RoundingStrategy::ToZero)
}

// if before market start left <= grace_seconds, we can't trade
pub fn allow_trade(market_timestamp: i64, grace_seconds: i64) -> bool {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time went backwards")
        .as_secs() as i64;

    now < market_timestamp - grace_seconds
}

pub fn nearest_quarter_hour() -> i64 {
    let now = Local::now();

    let minute = now.minute();
    let next_quarter_minute = ((minute / 15) + 1) * 15;

    let quarter = if next_quarter_minute < 60 {
        now.with_minute(next_quarter_minute)
            .unwrap()
            .with_second(0)
            .unwrap()
            .with_nanosecond(0)
            .unwrap()
    } else {
        (now + Duration::from_hours(1))
            .with_minute(0)
            .unwrap()
            .with_second(0)
            .unwrap()
            .with_nanosecond(0)
            .unwrap()
    };

    quarter.timestamp()
}

pub async fn get_tokens(
    http_client: &http_client,
    timestamp: &i64,
    asset: Asset,
) -> Result<MarketResponse, reqwest::Error> {
    let url =
        format!("https://gamma-api.polymarket.com/markets/slug/{asset}-updown-15m-{timestamp}");
    let resp = http_client.get(&url).send().await?;

    let api_resp: MarketApiResponse = resp.json().await?;

    let tokens: Vec<String> = api_resp
        .clob_token_ids
        .trim_matches(|c| c == '[' || c == ']')
        .split(',')
        .map(|s| s.trim().trim_matches('"').to_string())
        .collect();

    Ok(MarketResponse {
        first_asset_id: tokens[0].clone(),
        second_asset_id: tokens[1].clone(),
    })
}

pub async fn close_order_by_market(
    client: &Arc<Client<Authenticated<Normal>>>,
    signer: &LocalSigner<SigningKey>,
    token_id: &String,
    amount: Decimal,
) -> polymarket_client_sdk::Result<PostOrderResponse> {
    let market_order = client
        .market_order()
        .token_id(token_id)
        .amount(Amount::shares(amount)?)
        .side(Side::Sell)
        .order_type(OrderType::FOK)
        .build()
        .await?;
    let signed_order = client.sign(signer, market_order).await?;
    println!("closing position by market order",);
    let result = client.post_order(signed_order).await?;

    Ok(result[0].clone())
}

pub async fn place_hedge_order(
    client: &Arc<Client<Authenticated<Normal>>>,
    signer: &LocalSigner<SigningKey>,
    token_id: String,
    order_size: Decimal,
    price: Decimal,
) -> polymarket_client_sdk::Result<OrderResponse> {
    let order = client
        .limit_order()
        .token_id(&token_id)
        .size(order_size)
        .price(price)
        .side(Side::Buy)
        .order_type(OrderType::GTC)
        .build()
        .await?;

    let signed_order = client.sign(signer, order).await?;
    let response = client.post_order(signed_order).await?;
    Ok(OrderResponse {
        token_id: token_id,
        order_id: response[0].order_id.clone(),
    })
}

pub async fn open_start_positions(
    client: &Arc<Client<Authenticated<Normal>>>,
    signer: &LocalSigner<SigningKey>,
    order_size: Decimal,
    price: Decimal,
    tokens: MarketResponse,
) -> polymarket_client_sdk::Result<Option<Vec<OrderResponse>>> {
    let mut orders: Vec<OrderResponse> = vec![];
    let first_order = client
        .limit_order()
        .token_id(&tokens.first_asset_id)
        .size(order_size)
        .price(price)
        .side(Side::Buy)
        .order_type(OrderType::GTC)
        .build()
        .await?;

    let signed_order = client.sign(signer, first_order).await?;
    let response = client.post_order(signed_order).await?;
    orders.push(OrderResponse {
        token_id: tokens.first_asset_id,
        order_id: response[0].order_id.clone(),
    });

    sleep(Duration::from_secs(1)).await;

    let second_order = client
        .limit_order()
        .token_id(&tokens.second_asset_id)
        .size(order_size)
        .price(price)
        .side(Side::Buy)
        .order_type(OrderType::GTC)
        .build()
        .await?;

    let signed_order = client.sign(signer, second_order).await?;
    let response = client.post_order(signed_order).await?;
    orders.push(OrderResponse {
        token_id: tokens.second_asset_id,
        order_id: response[0].order_id.clone(),
    });

    Ok(Some(orders))
}

pub async fn get_asset_price(
    client: &Client<Authenticated<Normal>>,
    token_id: &str,
) -> polymarket_client_sdk::Result<PriceResponse> {
    let price_request = PriceRequestBuilder::default()
        .token_id(token_id)
        .side(Side::Sell)
        .build()?;

    client.price(&price_request).await
}
