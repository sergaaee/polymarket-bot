use alloy::signers::Signer as _;
use alloy::signers::local::LocalSigner;
use alloy_primitives::Address;

use common::*;
use polymarket_client_sdk::clob::{Client, Config};
use polymarket_client_sdk::types::{
    OpenOrderResponse, PostOrderResponse, PriceResponse, SignatureType,
};
use polymarket_client_sdk::{POLYGON, PRIVATE_KEY_VAR};
use reqwest::Client as http_client;
use rust_decimal::Decimal;
use rust_decimal::prelude::Zero;
use std::str::FromStr as _;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::sleep;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();

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
    let max_loss = std::env::var("MAX_LOSS").expect("Need a max loss");
    let max_loss = Decimal::from_str_exact(max_loss.as_str())
        .expect("Max loss must be a valid decimal number");
    let timing: u32 = std::env::var("TIMING")
        .expect("Need a timing")
        .parse()
        .expect("TIMING must be an integer");
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

    loop {
        let timestamp = nearest_quarter_hour();
        if !allow_trade(timestamp, 90) {
            println!("Not time to trade already, sleeping for 30 seconds");
            sleep(Duration::from_secs(30)).await;
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
                Ok(Some(orders)) => {
                    println!("Opened positions: {:?}", orders);
                    let first_order: OrderResponse = orders[0].clone();
                    let second_order: OrderResponse = orders[1].clone();
                    sleep(Duration::from_secs(10)).await;
                    loop {
                        sleep(Duration::from_secs(1)).await;
                        let first_order_id = first_order.order_id.clone();
                        let second_order_id = second_order.order_id.clone();
                        let first_order =
                            get_order_with_retry(&client, &first_order_id.as_str(), 10).await?;
                        let second_order =
                            get_order_with_retry(&client, &second_order_id.as_str(), 10).await?;

                        // if left lest than grace_seconds till market open we don't want to wait anymore to open positions
                        let is_holding_allowed = allow_trade(timestamp, 10);
                        println!(
                            "Holding allowed: {}, first: {}, second: {}",
                            is_holding_allowed, first_order.status, second_order.status
                        );

                        if first_order.status == "MATCHED" {
                            println!("First order matched: {:?}", first_order);
                            let close_size = normalized_size(first_order.size_matched, order_size);
                            let result = handle_matched(
                                &client,
                                &signer,
                                &second_order_id,
                                HedgeConfig {
                                    second_order_id: second_order_id.clone(),
                                    hedge_asset_id: tokens.second_asset_id.clone(),
                                    initial_asset_id: tokens.first_asset_id.clone(),
                                    hedge_size: order_size,
                                    close_size,
                                    hedge_enter_price,
                                    timestamp,
                                },
                            )
                                .await?;

                            match result.signum() {
                                1 => win_count += 1,
                                -1 => loss_count += 1,
                                _ => {}
                            }
                            break;
                        }

                        if second_order.status == "MATCHED" {
                            println!("Second order matched: {:?}", second_order);
                            let close_size = normalized_size(second_order.size_matched, order_size);
                            let result = handle_matched(
                                &client,
                                &signer,
                                &first_order_id,
                                HedgeConfig {
                                    second_order_id: first_order_id.clone(),
                                    hedge_asset_id: tokens.first_asset_id.clone(),
                                    initial_asset_id: tokens.second_asset_id.clone(),
                                    hedge_size: order_size,
                                    close_size,
                                    hedge_enter_price,
                                    timestamp,
                                },
                            )
                                .await?;

                            match result.signum() {
                                1 => win_count += 1,
                                -1 => loss_count += 1,
                                _ => {}
                            }
                            break;
                        }
                        if first_order.status == "CANCELED" && second_order.status == "CANCELED" {
                            println!(
                                "Orders were canceled: first: {:?}, second: {:?}",
                                first_order, second_order
                            );
                            break;
                        }

                        // --- PREVENT HOLDING ---
                        if !is_holding_allowed {
                            if first_order.status == "LIVE" {
                                let size = normalized_size(first_order.size_matched, order_size);
                                let exited = handle_live_order(
                                    &client,
                                    &signer,
                                    &first_order,
                                    HedgeConfig {
                                        second_order_id: second_order_id.clone(),
                                        hedge_asset_id: tokens.second_asset_id.clone(),
                                        initial_asset_id: tokens.first_asset_id.clone(),
                                        hedge_size: size,
                                        close_size: size,
                                        hedge_enter_price,
                                        timestamp,
                                    },
                                    &first_order_id,
                                )
                                    .await?;
                                if exited {
                                    break 'open_position;
                                }
                            }

                            if second_order.status == "LIVE" {
                                let size = normalized_size(second_order.size_matched, order_size);
                                let exited = handle_live_order(
                                    &client,
                                    &signer,
                                    &second_order,
                                    HedgeConfig {
                                        second_order_id: first_order_id.clone(),
                                        hedge_asset_id: tokens.first_asset_id.clone(),
                                        initial_asset_id: tokens.second_asset_id.clone(),
                                        hedge_size: size,
                                        close_size: size,
                                        hedge_enter_price,
                                        timestamp,
                                    },
                                    &second_order_id,
                                )
                                    .await?;
                                if exited {
                                    break 'open_position;
                                }
                            }
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
