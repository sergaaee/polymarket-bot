use alloy::signers::Signer as _;
use alloy::signers::local::LocalSigner;
use alloy_primitives::Address;
use std::env;

use common::*;
use polymarket_client_sdk::clob::types::{OrderStatusType, SignatureType};
use polymarket_client_sdk::clob::{Client, Config};
use polymarket_client_sdk::{POLYGON, PRIVATE_KEY_VAR};
use prometheus::{Encoder, TextEncoder};
use reqwest::Client as http_client;
use rust_decimal::Decimal;
use std::str::FromStr as _;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::sleep;

fn get_metrics_port() -> u16 {
    env::var("METRICS_PORT")
        .unwrap_or_else(|_| "9101".to_string()) // дефолтный порт
        .parse()
        .expect("METRICS_PORT must be a valid number")
}

async fn metrics_handler() -> String {
    let encoder = TextEncoder::new();
    let metric_families = prometheus::gather();

    let mut buffer = Vec::new();
    encoder.encode(&metric_families, &mut buffer).unwrap();

    String::from_utf8(buffer).unwrap()
}

fn start_metrics_server(port: u16) {
    tokio::spawn(async move {
        let app = axum::Router::new().route("/metrics", axum::routing::get(metrics_handler));

        let addr = std::net::SocketAddr::from(([0, 0, 0, 0], port));
        println!("📊 Metrics server started on {}", addr);

        let listener = tokio::net::TcpListener::bind(addr)
            .await
            .expect("Failed to bind metrics port");
        axum::serve(listener, app)
            .await
            .expect("Metrics server crashed");
    });
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();

    let port = get_metrics_port();
    start_metrics_server(port);

    let private_key = std::env::var(PRIVATE_KEY_VAR).expect("Need a private key");
    let funder_addr = std::env::var("PM_ADDRESS").expect("Need a funder address");
    let order_size = std::env::var("ORDER_SIZE").expect("Need an order size");
    let order_size = Decimal::from_str_exact(order_size.as_str())
        .expect("Order size must be a valid decimal number");
    let limit_enter_price = std::env::var("LIMIT_ENTER_PRICE").expect("Need a limit enter price");
    let limit_enter_price = Decimal::from_str_exact(limit_enter_price.as_str())
        .expect("Limit enter price must be a valid decimal number");
    let hedge_enter_price = std::env::var("HEDGE_ENTER_PRICE").expect("Need a hedge enter price");
    let hedge_enter_price = Decimal::from_str_exact(hedge_enter_price.as_str())
        .expect("Hedge enter price must be a valid decimal number");
    let dont_allow_trade_before = std::env::var("DONT_ALLOW_TRADE_BEFORE")
        .expect("Need a time to stop trading")
        .parse::<i64>()
        .expect("DONT_ALLOW_TRADE_BEFORE must be i64");
    let dont_allow_holding_before = std::env::var("DONT_ALLOW_HOLDING_BEFORE")
        .expect("Need a time to stop holding")
        .parse::<i64>()
        .expect("DONT_ALLOW_HOLDING_BEFORE must be i64");
    let stop_loss_after = std::env::var("STOP_LOSS_AFTER")
        .expect("Need a stop loss after")
        .parse::<i64>()
        .expect("STOP_LOSS_AFTER must be i64");
    let address = Address::parse_checksummed(funder_addr, None).expect("valid checksum");
    let http_client = http_client::new();
    let signer = LocalSigner::from_str(&private_key)?.with_chain_id(Some(POLYGON));
    let client = Arc::new(
        Client::new("https://clob.polymarket.com", Config::default())?
            .authentication_builder(&signer)
            .funder(address)
            .signature_type(SignatureType::GnosisSafe)
            .authenticate()
            .await?,
    );

    let ok = client.ok().await?;
    println!("Client setup ok?: {ok}");
    let mut win_count: u32 = 0;
    let mut retries_count: u32 = 0;
    let mut completed_timestamps: Vec<i64> = Vec::new();

    loop {
        let timestamp = current_quarter_hour();

        if !allow_trade(timestamp, &800) || completed_timestamps.contains(&timestamp) {
            println!("Not yet. Sleeping for 1 second.");
            sleep(Duration::from_secs(1)).await;
            continue;
        }

        let tokens = get_tokens(&http_client, &timestamp, Asset::ETH)
            .await
            .expect(
                "Failed to get tokens from API. Please check your network connection and try again later.",
            );

        loop {
            println!(
                "Waiting for {} price to up above 0.95... (sleeping for 1 second)",
                Asset::ETH
            );
            let first_token_price = get_asset_price(&client, &tokens.first_asset_id)
                .await?
                .price;
            if first_token_price >= Decimal::from_str_exact("0.95").unwrap() {
                while retries_count < 100 {
                    match open_position_by_market(
                        &client,
                        &signer,
                        &tokens.first_asset_id,
                        order_size,
                    )
                        .await
                    {
                        Ok(position) => {
                            println!("Opened position: {:?}", position);
                            win_count += 1;
                            completed_timestamps.push(timestamp);
                            retries_count = 0;
                            break;
                        }
                        Err(err) => {
                            retries_count += 1;
                            println!("Failed to open position {retries_count}/100: {}", err);
                            sleep(Duration::from_secs(1)).await;
                        }
                    }
                }
                break;
            }
            let second_token_price = get_asset_price(&client, &tokens.second_asset_id)
                .await?
                .price;
            if second_token_price >= Decimal::from_str_exact("0.95").unwrap() {
                while retries_count < 100 {
                    match open_position_by_market(
                        &client,
                        &signer,
                        &tokens.second_asset_id,
                        order_size,
                    )
                        .await
                    {
                        Ok(position) => {
                            println!("Opened position: {:?}", position);
                            win_count += 1;
                            completed_timestamps.push(timestamp);
                            retries_count = 0;
                            break;
                        }
                        Err(err) => {
                            retries_count += 1;
                            println!("Failed to open position {retries_count}/100: {}", err);
                            sleep(Duration::from_secs(1)).await;
                        }
                    }
                }
                break;
            }
            sleep(Duration::from_secs(1)).await;
        }
        println!("win count: {} | {}", win_count, Asset::ETH);
    }
}
