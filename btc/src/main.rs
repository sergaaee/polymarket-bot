use alloy::signers::Signer as _;
use alloy::signers::local::LocalSigner;
use alloy_primitives::Address;

use common::*;
use polymarket_client_sdk::clob::{Client, Config};
use polymarket_client_sdk::types::{OpenOrderResponse, PriceResponse, SignatureType};
use polymarket_client_sdk::{POLYGON, PRIVATE_KEY_VAR};
use reqwest::Client as http_client;
use rust_decimal::Decimal;
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
    let mut completed_timestamps: Vec<i64> = vec![];
    let mut win_count: u32 = 0;
    let mut loss_count: u32 = 0;

    loop {
        let timestamp = nearest_quarter_hour();
        let tokens = get_tokens(&http_client, &timestamp, Asset::BTC)
            .await
            .expect(
                "Failed to get tokens from API. Please check your network connection and try again later.",
            );

        println!("win count: {}, loss count: {} | {}", win_count, loss_count, Asset::BTC);

        // skip if we already completed this timestamp
        // if completed_timestamps.contains(&timestamp) {
        //     println!("Already completed timestamp: {}", timestamp);
        //     continue;
        // }
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
                        let first_order_status: OpenOrderResponse =
                            client.order(&first_order.order_id.as_str()).await?;
                        println!("First order status: {:?}", first_order_status.status);
                        if first_order_status.status == "MATCHED" {
                            println!("First order matched");
                            client.cancel_order(&second_order.order_id).await?;
                            println!("Second order canceled, opening hedge order,,,");
                            let hedge_order: OrderResponse = place_hedge_order(
                                &client,
                                &signer,
                                tokens.second_asset_id.clone(),
                                order_size,
                                hedge_enter_price,
                            )
                            .await?;
                            println!("Hedge order placed");
                            sleep(Duration::from_secs(10)).await;
                            'hedge_order_loop: loop {
                                let hedge_order_status: OpenOrderResponse =
                                    client.order(&hedge_order.order_id.as_str()).await?;
                                println!("Hedge order status: {:?}", hedge_order_status.status);
                                if hedge_order_status.status == "MATCHED" {
                                    println!("Hedge order matched");
                                    win_count += 1;
                                    break 'hedge_order_loop;
                                }
                                sleep(Duration::from_secs(1)).await;

                                if hedge_order_status.status != "MATCHED"
                                    && allow_stop_loss(timestamp, 20)
                                {
                                    println!(
                                        "Stop loss reached, cancelling hedge order and closing position..."
                                    );
                                    client.cancel_order(&hedge_order.order_id.as_str()).await?;
                                    println!("Hedge order canceled");
                                    let closed_order = close_order_by_market(
                                        &client,
                                        &signer,
                                        tokens.first_asset_id,
                                        order_size,
                                    )
                                        .await?;
                                    println!("Initial position closed: {:?}", closed_order);
                                    loss_count += 1;
                                    break 'hedge_order_loop;
                                }
                            }
                            break;
                        }
                        sleep(Duration::from_secs(1)).await;
                        let second_order_status: OpenOrderResponse =
                            client.order(&second_order.order_id.as_str()).await?;
                        println!("Second order status: {:?}", second_order_status.status);
                        if second_order_status.status == "MATCHED" {
                            println!("Second order matched");
                            client.cancel_order(&first_order.order_id).await?;
                            println!("First order canceled, opening hedge order,,,");
                            let hedge_order: OrderResponse = place_hedge_order(
                                &client,
                                &signer,
                                tokens.first_asset_id.clone(),
                                order_size,
                                hedge_enter_price,
                            )
                            .await?;
                            println!("Hedge order placed");
                            sleep(Duration::from_secs(10)).await;
                            'hedge_order_loop: loop {
                                let hedge_order_status: OpenOrderResponse =
                                    client.order(&hedge_order.order_id.as_str()).await?;
                                println!("Hedge order status: {:?}", hedge_order_status.status);
                                if hedge_order_status.status == "MATCHED" {
                                    println!("Hedge order matched");
                                    win_count += 1;
                                    break 'hedge_order_loop;
                                }
                                sleep(Duration::from_secs(1)).await;
                                let current_first_position: PriceResponse =
                                    get_asset_price(&client, tokens.second_asset_id.as_str())
                                        .await?;
                                if hedge_order_status.status != "MATCHED"
                                    && allow_stop_loss(timestamp, 20)
                                {
                                    println!(
                                        "Stop loss reached, cancelling hedge order and closing position..."
                                    );
                                    client.cancel_order(&hedge_order.order_id.as_str()).await?;
                                    println!("Hedge order canceled");
                                    let closed_order = close_order_by_market(
                                        &client,
                                        &signer,
                                        tokens.second_asset_id,
                                        order_size,
                                    )
                                        .await?;
                                    println!("Initial position closed: {:?}", closed_order);
                                    loss_count += 1;
                                    break 'hedge_order_loop;
                                }
                            }
                            break;
                        }
                    }
                    completed_timestamps.push(timestamp);
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
