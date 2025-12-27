use crate::dto::{Asset, OrderResponse};
use crate::metrics::{
    HEDGE_ORDERS_CANCELLED_TOTAL, HEDGE_ORDERS_MATCHED_TOTAL, HEDGE_ORDERS_PARTIAL_TOTAL,
    HEDGE_ORDERS_TOTAL, ORDERS_CANCELLED_TOTAL, ORDERS_MATCHED_TOTAL, ORDERS_PARTIAL_TOTAL,
    ORDERS_TOTAL, REQUEST_LATENCY, RETRIES_TOTAL, STOP_LOSS_TOTAL,
};
use crate::{HedgeConfig, MarketApiResponse, MarketResponse, PreventHoldingConfig};
use alloy::signers::k256::ecdsa::SigningKey;
use alloy::signers::k256::ecdsa::signature::SignerMut;
use alloy::signers::local::LocalSigner;
use chrono::{Local, TimeZone, Timelike};
use polymarket_client_sdk::auth::Normal;
use polymarket_client_sdk::clob::Client;
use polymarket_client_sdk::clob::state::Authenticated;
use polymarket_client_sdk::types::{
    Amount, OpenOrderResponse, OrderType, PostOrderResponse, PriceRequestBuilder, PriceResponse,
    Side,
};
use reqwest::Client as http_client;
use rust_decimal::prelude::Zero;
use rust_decimal::{Decimal, RoundingStrategy};
use std::str::FromStr;
use std::sync::Arc;
use std::time::Instant;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::time::sleep;

pub async fn timed_request<F, T>(service: &str, method: &str, f: F) -> T
where
    F: Future<Output = T>,
{
    let start = Instant::now();
    let result = f.await;
    let elapsed = start.elapsed().as_secs_f64();

    REQUEST_LATENCY
        .with_label_values(&[service, method])
        .observe(elapsed);

    result
}

/// if current time > grace_second we count it as a stop-loss
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

pub async fn close_position_with_retry(
    client: &Arc<Client<Authenticated<Normal>>>,
    signer: &LocalSigner<SigningKey>,
    asset_id: &String,
    close_size: Decimal,
    max_retries: usize,
    asset: &Asset,
) -> Option<PostOrderResponse> {
    let mut attempt = 0;

    loop {
        let response = close_position_by_market(&client, &signer, asset_id, close_size)
            .await
            .ok()?;

        match response.error_msg.as_deref() {
            Some("") | None => {
                // успех
                return Some(response);
            }
            Some(err) => {
                attempt += 1;
                RETRIES_TOTAL
                    .with_label_values(&[asset.to_string().as_str(), "close_position"])
                    .inc();

                if attempt >= max_retries {
                    return None;
                }
                println!(
                    "close_order failed (attempt {}/{}): {}",
                    attempt, max_retries, err
                );

                sleep(Duration::from_millis(1000)).await;
            }
        }
    }
}

pub async fn get_order_with_retry(
    client: &Arc<Client<Authenticated<Normal>>>,
    order_id: &str,
    max_retries: usize,
    asset: &Asset,
) -> polymarket_client_sdk::Result<OpenOrderResponse> {
    let mut attempt = 0;

    loop {
        match timed_request("polymarket", "get_order", client.order(order_id)).await {
            Ok(status) => return Ok(status),

            Err(err) => {
                attempt += 1;
                RETRIES_TOTAL
                    .with_label_values(&[asset.to_string().as_str(), "get_order"])
                    .inc();

                if attempt >= max_retries {
                    return Err(err);
                }

                println!(
                    "get_order failed (attempt {}/{}): {}",
                    attempt, max_retries, err
                );

                sleep(Duration::from_millis(1000)).await;
            }
        }
    }
}

pub async fn handle_matched(
    client: &Arc<Client<Authenticated<Normal>>>,
    signer: &LocalSigner<SigningKey>,
    cancel_order_id: &str,
    hedge_config: HedgeConfig,
) -> polymarket_client_sdk::Result<i8> {
    ORDERS_TOTAL
        .with_label_values(&[&hedge_config.asset.to_string()])
        .inc();
    ORDERS_TOTAL
        .with_label_values(&[&hedge_config.asset.to_string()])
        .inc();

    ORDERS_MATCHED_TOTAL
        .with_label_values(&[&hedge_config.asset.to_string()])
        .inc();

    println!("Cancelling another order...");
    timed_request(
        "polymarket",
        "cancel_order",
        client.cancel_order(cancel_order_id),
    )
    .await?;
    manage_position_after_match(client, signer, hedge_config).await
}

pub async fn handle_live_order(
    client: &Arc<Client<Authenticated<Normal>>>,
    signer: &LocalSigner<SigningKey>,
    status: &OpenOrderResponse,
    hedge_config: HedgeConfig,
    cancel_order_id: &str,
) -> polymarket_client_sdk::Result<bool> {
    ORDERS_TOTAL
        .with_label_values(&[&hedge_config.asset.to_string()])
        .inc();

    if !status.size_matched.is_zero() {
        ORDERS_PARTIAL_TOTAL
            .with_label_values(&[&hedge_config.asset.to_string()])
            .inc();
        prevent_holding_position(
            client,
            signer,
            PreventHoldingConfig {
                hedge_config,
                order_id: cancel_order_id.to_string(),
            },
        )
        .await?;
        Ok(true)
    } else {
        ORDERS_CANCELLED_TOTAL
            .with_label_values(&[&hedge_config.asset.to_string()])
            .inc();

        println!("No open position, going to cancel it");
        timed_request(
            "polymarket",
            "cancel_order",
            client.cancel_order(cancel_order_id),
        )
        .await?;
        Ok(false)
    }
}

pub async fn prevent_holding_position(
    client: &Arc<Client<Authenticated<Normal>>>,
    signer: &LocalSigner<SigningKey>,
    prevent_holding_config: PreventHoldingConfig,
) -> polymarket_client_sdk::Result<()> {
    ORDERS_CANCELLED_TOTAL
        .with_label_values(&[&prevent_holding_config.hedge_config.asset.to_string()])
        .inc();

    timed_request(
        "polymarket",
        "cancel_order",
        client.cancel_order(&prevent_holding_config.order_id),
    )
    .await?;
    println!("Cancelled first order, closing now");

    let first_order_status: OpenOrderResponse = get_order_with_retry(
        &client,
        &prevent_holding_config.order_id.as_str(),
        30,
        &prevent_holding_config.hedge_config.asset,
    )
    .await?;
    let first_order_size = normalized_size(
        first_order_status.size_matched,
        prevent_holding_config.hedge_config.hedge_size,
    );
    println!(
        "Time's up to wait for first order opening, going to open hedge with size = {}",
        &first_order_size
    );
    let true_hedge_config = HedgeConfig {
        initial_entry_price: prevent_holding_config.hedge_config.initial_entry_price,
        second_order_id: prevent_holding_config.hedge_config.second_order_id,
        hedge_asset_id: prevent_holding_config.hedge_config.hedge_asset_id,
        initial_asset_id: prevent_holding_config.hedge_config.initial_asset_id,
        hedge_size: first_order_size,
        hedge_enter_price: prevent_holding_config.hedge_config.hedge_enter_price,
        close_size: first_order_size,
        timestamp: prevent_holding_config.hedge_config.timestamp,
        asset: prevent_holding_config.hedge_config.asset,
    };
    manage_position_after_match(client, signer, true_hedge_config.clone()).await?;
    Ok(())
}

pub fn normalized_size(size: Decimal, fallback: Decimal) -> Decimal {
    let s = floor_dp(size, 2);
    if s.is_zero() {
        println!("Retrieved bad data from polymarket about size: {}", s);
        fallback
    } else {
        s
    }
}

async fn complete_hedging(
    client: &Arc<Client<Authenticated<Normal>>>,
    signer: &LocalSigner<SigningKey>,
) {
    todo!()
}

pub async fn manage_position_after_match(
    client: &Arc<Client<Authenticated<Normal>>>,
    signer: &LocalSigner<SigningKey>,
    hedge_config: HedgeConfig,
) -> polymarket_client_sdk::Result<i8> {
    let second_order_status: OpenOrderResponse = get_order_with_retry(
        client,
        hedge_config.second_order_id.as_str(),
        30,
        &hedge_config.asset,
    )
    .await?;
    if second_order_status.status != "CANCELED" {
        println!("Cancelling second order...");
        ORDERS_CANCELLED_TOTAL
            .with_label_values(&[&hedge_config.asset.to_string()])
            .inc();

        timed_request(
            "polymarket",
            "cancel_order",
            client.cancel_order(hedge_config.second_order_id.as_str()),
        )
        .await?;
        println!("Second order cancelled");
    }
    let second_order_status: OpenOrderResponse = get_order_with_retry(
        client,
        hedge_config.second_order_id.as_str(),
        30,
        &hedge_config.asset,
    )
    .await?;
    let mut hedge_size = hedge_config.hedge_size;
    if second_order_status.size_matched > Decimal::zero() {
        ORDERS_PARTIAL_TOTAL
            .with_label_values(&[&hedge_config.asset.to_string()])
            .inc();

        let closing_second_size = floor_dp(second_order_status.size_matched, 2);
        println!(
            "Second order partially matched with size: {}",
            &closing_second_size
        );
        hedge_size = closing_second_size - hedge_config.hedge_size;
        if hedge_size < Decimal::zero() {
            hedge_size = hedge_config.hedge_size - closing_second_size;
        }
    }

    let hedge_order: OrderResponse = place_hedge_order(
        &client,
        &signer,
        &hedge_config.hedge_asset_id,
        hedge_size,
        hedge_config.hedge_enter_price,
        &hedge_config.asset,
        OrderType::GTC,
    )
    .await?;
    println!("Hedge order placed: {:?}", hedge_order);
    sleep(Duration::from_secs(10)).await;

    loop {
        let hedge_order_status: OpenOrderResponse = get_order_with_retry(
            client,
            hedge_order.order_id.as_str(),
            20,
            &hedge_config.asset,
        )
        .await?;
        println!("Hedge order status: {:?}", hedge_order_status.status);
        if hedge_order_status.status == "MATCHED" {
            HEDGE_ORDERS_MATCHED_TOTAL
                .with_label_values(&[&hedge_config.asset.to_string()])
                .inc();

            println!("Hedge order matched");
            return Ok(1);
        }
        sleep(Duration::from_secs(1)).await;
        if hedge_order_status.status != "MATCHED" && allow_stop_loss(hedge_config.timestamp, 60) {
            STOP_LOSS_TOTAL
                .with_label_values(&[&hedge_config.asset.to_string()])
                .inc();

            println!("Stop loss reached, cancelling hedge order and closing position...");
            timed_request(
                "polymarket",
                "cancel_order",
                client.cancel_order(&hedge_order.order_id.as_str()),
            )
            .await?;

            HEDGE_ORDERS_CANCELLED_TOTAL
                .with_label_values(&[&hedge_config.asset.to_string()])
                .inc();
            println!("Hedge order canceled");
            loop {
                let current_second_asset_price = timed_request(
                    "polymarket",
                    "get_price",
                    get_asset_price(client, &hedge_config.hedge_asset_id),
                )
                .await?
                .price;
                let closing_hedge_size = normalized_size(
                    (hedge_config.close_size * hedge_config.initial_entry_price)
                        / (Decimal::ONE - current_second_asset_price),
                    Decimal::zero(),
                );
                let hedge_order: OrderResponse = timed_request(
                    "polymarket",
                    "place_hedge_order",
                    place_hedge_order(
                        client,
                        signer,
                        &hedge_config.hedge_asset_id,
                        closing_hedge_size,
                        current_second_asset_price,
                        &hedge_config.asset,
                        OrderType::FOK,
                    ),
                )
                .await?;
                sleep(Duration::from_secs(5)).await;
                let hedge_order_status: OpenOrderResponse = get_order_with_retry(
                    client,
                    hedge_order.order_id.as_str(),
                    20,
                    &hedge_config.asset,
                )
                .await?;
                if hedge_order_status.status == "MATCHED" {
                    return Ok(1);
                }
            }

            // sleep(Duration::from_secs(1)).await;
            // let hedge_order_status: OpenOrderResponse =
            //     get_order_with_retry(client, hedge_order.order_id.as_str(), 10, &hedge_config.asset).await?;
            // if hedge_order_status.size_matched > Decimal::zero()
            //     && hedge_order_status.size_matched != hedge_size
            // {
            //     HEDGE_ORDERS_PARTIAL_TOTAL
            //         .with_label_values(&[&hedge_config.asset.to_string()])
            //         .inc();
            //
            //     println!("Hedge order partially matched, closing it...");
            //     let closing_hedge_size =
            //         normalized_size(hedge_order_status.size_matched, hedge_size);
            //     if let Some(closed_order) = close_position_with_retry(
            //         client,
            //         signer,
            //         &hedge_config.hedge_asset_id,
            //         closing_hedge_size,
            //         30,
            //         &hedge_config.asset
            //     )
            //     .await
            //     {
            //         println!(
            //             "Hedge order after partially filling closed: {:?}",
            //             closed_order
            //         );
            //     } else {
            //         println!("Failed to close hedge order");
            //     }
            // }
            //
            // if let Some(closed_order) = close_position_with_retry(
            //     client,
            //     signer,
            //     &hedge_config.initial_asset_id,
            //     hedge_config.close_size,
            //     30,
            //     &hedge_config.asset
            // )
            // .await
            // {
            //     println!("Initial position closed after sl: {:?}", closed_order);
            //     return Ok(-1);
            // } else {
            //     println!("Failed to close initial position");
            //     return Ok(0);
            // }
        }
    }
}

// if before market start left <= grace_seconds, we can't open new positions
pub fn allow_trade(market_timestamp: i64, grace_seconds: i64) -> bool {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time went backwards")
        .as_secs() as i64;

    now <= market_timestamp - grace_seconds
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

pub async fn close_position_by_market(
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
    let result = timed_request(
        "polymarket",
        "close_position_by_market",
        client.post_order(signed_order),
    )
    .await?;

    Ok(result[0].clone())
}

pub async fn place_hedge_order(
    client: &Arc<Client<Authenticated<Normal>>>,
    signer: &LocalSigner<SigningKey>,
    token_id: &String,
    order_size: Decimal,
    price: Decimal,
    asset: &Asset,
    order_type: OrderType,
) -> polymarket_client_sdk::Result<OrderResponse> {
    HEDGE_ORDERS_TOTAL
        .with_label_values(&[asset.to_string().as_str()])
        .inc();

    let order = client
        .limit_order()
        .token_id(token_id)
        .size(order_size)
        .price(price)
        .side(Side::Buy)
        .order_type(order_type)
        .build()
        .await?;

    let signed_order = client.sign(signer, order).await?;
    let response = timed_request(
        "polymarket",
        "place_hedge_order",
        client.post_order(signed_order),
    )
    .await?;

    Ok(OrderResponse {
        token_id: token_id.to_string(),
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
    let response = timed_request(
        "polymarket",
        "open_first_start_positions",
        client.post_order(signed_order),
    )
    .await?;
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
    let response = timed_request(
        "polymarket",
        "open_second_start_positions",
        client.post_order(signed_order),
    )
    .await?;
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
        .side(Side::Buy)
        .build()?;

    timed_request("polymarket", "price", client.price(&price_request)).await
}
