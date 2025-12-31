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
    let mut loss_count: u32 = 0;
    let mut completed_timesteps: Vec<i64> = vec![];

    loop {
        let timestamp = current_quarter_hour();
        if completed_timesteps.contains(&timestamp) {
            println!("Already completed timestamp: {}", timestamp);
            sleep(Duration::from_secs(1)).await;
            continue;
        }
        let tokens = get_tokens(&http_client, &timestamp, Asset::ETH)
            .await
            .expect(
                "Failed to get tokens from API. Please check your network connection and try again later.",
            );

        println!(
            "win count: {}, loss count: {} | {}",
            win_count,
            loss_count,
            Asset::ETH
        );
        let mut hedge_asset_id;
        let mut initial_asset_id;

        'open_position: loop {
            match open_start_positions(
                &client,
                &signer,
                order_size,
                limit_enter_price,
                tokens.clone(),
            )
                .await
            {
                Ok(Some(order)) => {
                    println!("Opened order: {:?}", order);
                    sleep(Duration::from_secs(8)).await;
                    loop {
                        sleep(Duration::from_secs(1)).await;
                        let order_id = order.order_id.clone();
                        let first_order =
                            get_order_with_retry(&client, &order_id.as_str(), 20, &Asset::ETH)
                                .await?;
                        if order.token_id == tokens.first_asset_id.clone() {
                            initial_asset_id = tokens.first_asset_id.clone();
                            hedge_asset_id = tokens.second_asset_id.clone();
                        } else {
                            initial_asset_id = tokens.second_asset_id.clone();
                            hedge_asset_id = tokens.first_asset_id.clone();
                        }

                        // if left lest than grace_seconds till market open we don't want to wait anymore to open positions

                        if first_order.status == OrderStatusType::Matched {
                            println!("First order matched: {:?}", first_order);
                            let close_size = normalized_size(first_order.size_matched, order_size);
                            let result = handle_matched(
                                &client,
                                &signer,
                                HedgeConfig {
                                    stop_loss_after,
                                    hedge_asset_id,
                                    initial_asset_id,
                                    hedge_size: order_size,
                                    close_size,
                                    hedge_enter_price,
                                    timestamp,
                                    asset: Asset::ETH,
                                },
                            )
                                .await?;

                            match result.signum() {
                                1 => win_count += 1,
                                -1 => loss_count += 1,
                                _ => {}
                            }
                            completed_timesteps.push(timestamp.clone());
                            break;
                        }
                    }
                    break 'open_position;
                }
                Ok(None) => {
                    // retry
                }
                Err(e) => {
                    eprintln!("Error opening positions: {e}");
                }
            }
            sleep(Duration::from_secs(1)).await;
        }
    }
}
