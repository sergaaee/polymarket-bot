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
    let mut completed_timestamps: Vec<i64> = vec![];
    let mut win_count: u32 = 0;
    let mut loss_count: u32 = 0;

    loop {
        let timestamp = nearest_quarter_hour();
        if !allow_trade(timestamp, 90) {
            println!("Not time to trade already");
            continue;
        }
        let tokens = get_tokens(&http_client, &timestamp, Asset::XRP)
            .await
            .expect(
                "Failed to get tokens from API. Please check your network connection and try again later.",
            );

        println!(
            "win count: {}, loss count: {} | {}",
            win_count,
            loss_count,
            Asset::XRP
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
                        let first_order_status: OpenOrderResponse =
                            client.order(&first_order.order_id.as_str()).await?;
                        let is_trade_allowed = allow_trade(timestamp, 30);
                        println!(
                            "Trade still allowed : {:?}, first order status: {:?}",
                            is_trade_allowed, first_order_status.status
                        );
                        if !is_trade_allowed
                            && first_order_status.status != "MATCHED"
                            && first_order_status.status != "CANCELED"
                        {
                            let has_open_position_first =
                                !first_order_status.size_matched.is_zero();
                            if has_open_position_first {
                                client.cancel_order(&first_order.order_id).await?;
                                println!("Cancelled first order, closing now");
                                let first_order_status: OpenOrderResponse =
                                    client.order(&first_order.order_id.as_str()).await?;
                                println!(
                                    "Time's up to wait for first order opening, going to close it with size = {}",
                                    first_order_status.size_matched
                                );
                                let closed_order: PostOrderResponse;
                                loop {
                                    let response = close_order_by_market(
                                        &client,
                                        &signer,
                                        &tokens.first_asset_id,
                                        first_order_status.size_matched,
                                    )
                                        .await?;

                                    match response.error_msg.as_deref() {
                                        Some("") | None => {
                                            // успех
                                            closed_order = response;
                                            println!("Initial position closed: {:?}", closed_order);
                                            break;
                                        }
                                        Some(err) => {
                                            println!("close order failed: {}", err);
                                            continue;
                                        }
                                    }
                                }
                            }
                        }
                        let second_order_status: OpenOrderResponse =
                            client.order(&second_order.order_id.as_str()).await?;
                        println!("Second order status: {:?}", second_order_status.status);
                        if !allow_trade(timestamp, 30)
                            && second_order_status.status != "MATCHED"
                            && second_order_status.status != "CANCELED"
                        {
                            let has_open_position_second =
                                !second_order_status.size_matched.is_zero();
                            if has_open_position_second {
                                client.cancel_order(&second_order.order_id).await?;
                                println!("Cancelled first order, closing now");
                                let second_order_status: OpenOrderResponse =
                                    client.order(&second_order.order_id.as_str()).await?;
                                println!(
                                    "Time's up to wait for second order opening, going to close it with size = {}",
                                    second_order_status.size_matched
                                );
                                let closed_order: PostOrderResponse;
                                loop {
                                    let response = close_order_by_market(
                                        &client,
                                        &signer,
                                        &tokens.second_asset_id,
                                        second_order_status.size_matched,
                                    )
                                        .await?;

                                    match response.error_msg.as_deref() {
                                        Some("") | None => {
                                            // успех
                                            closed_order = response;
                                            println!("Initial position closed: {:?}", closed_order);
                                            break;
                                        }
                                        Some(err) => {
                                            println!("close order failed: {}", err);
                                            continue;
                                        }
                                    }
                                }
                            }
                        }

                        if first_order_status.status == "MATCHED" {
                            println!("First order matched");
                            let close_size = first_order_status.size_matched;
                            println!("Close size will be = {}", close_size);
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

                                    let closed_order: PostOrderResponse;
                                    loop {
                                        let response = close_order_by_market(
                                            &client,
                                            &signer,
                                            &tokens.first_asset_id,
                                            close_size,
                                        )
                                            .await?;

                                        match response.error_msg.as_deref() {
                                            Some("") | None => {
                                                // успех
                                                closed_order = response;
                                                break;
                                            }
                                            Some(err) => {
                                                println!("close order failed: {}", err);
                                                continue;
                                            }
                                        }
                                    }

                                    println!("Initial position closed: {:?}", closed_order);
                                    loss_count += 1;
                                    break 'hedge_order_loop;
                                }
                            }
                            break;
                        }

                        if second_order_status.status == "MATCHED" {
                            println!("Second order matched");
                            let close_size = second_order_status.size_matched;
                            println!("Close size will be = {}", close_size);
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
                                if hedge_order_status.status != "MATCHED"
                                    && allow_stop_loss(timestamp, 20)
                                {
                                    println!(
                                        "Stop loss reached, cancelling hedge order and closing position..."
                                    );
                                    client.cancel_order(&hedge_order.order_id.as_str()).await?;
                                    println!("Hedge order canceled");

                                    let closed_order: PostOrderResponse;
                                    loop {
                                        let response = close_order_by_market(
                                            &client,
                                            &signer,
                                            &tokens.second_asset_id,
                                            close_size,
                                        )
                                            .await?;

                                        match response.error_msg.as_deref() {
                                            Some("") | None => {
                                                // успех
                                                closed_order = response;
                                                break;
                                            }
                                            Some(err) => {
                                                println!("close order failed: {}", err);
                                                continue;
                                            }
                                        }
                                    }

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
