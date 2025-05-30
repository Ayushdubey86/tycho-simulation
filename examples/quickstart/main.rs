use std::{
    collections::{HashMap, HashSet},
    default::Default,
    env,
    str::FromStr,
};

use alloy::{
    eips::BlockNumberOrTag,
    network::{Ethereum, EthereumWallet},
    providers::{
        fillers::{FillProvider, JoinFill, WalletFiller},
        Identity, Provider, ProviderBuilder, ReqwestProvider,
    },
    rpc::types::{
        simulate::{SimBlock, SimulatePayload},
        TransactionInput, TransactionRequest,
    },
    signers::local::PrivateKeySigner,
    transports::http::{Client, Http},
};
use alloy_primitives::{Address, Bytes as AlloyBytes, TxKind, B256, U256};
use alloy_sol_types::SolValue;
use clap::Parser;
use dialoguer::{theme::ColorfulTheme, Select};
use foundry_config::NamedChain;
use futures::StreamExt;
use num_bigint::BigUint;
use num_traits::ToPrimitive;
use tracing_subscriber::EnvFilter;
use tycho_common::Bytes;
pub mod utils;
use tycho_execution::encoding::{
    evm::{
        encoder_builder::EVMEncoderBuilder, tycho_encoder::EVMTychoEncoder, utils::encode_input,
    },
    models::{Solution, Swap, Transaction},
    tycho_encoder::TychoEncoder,
};
use tycho_simulation::{
    evm::{
        engine_db::tycho_db::PreCachedDB,
        protocol::{
            ekubo::state::EkuboState,
            filters::{balancer_pool_filter, uniswap_v4_pool_with_hook_filter},
            u256_num::biguint_to_u256,
            uniswap_v2::state::UniswapV2State,
            uniswap_v3::state::UniswapV3State,
            uniswap_v4::state::UniswapV4State,
            vm::state::EVMPoolState,
        },
        stream::ProtocolStreamBuilder,
    },
    models::Token,
    protocol::models::{BlockUpdate, ProtocolComponent},
    tycho_client::feed::component_tracker::ComponentFilter,
    tycho_common::models::Chain,
    utils::load_all_tokens,
};
use utils::get_default_url;

const FAKE_PK: &str = "0x123456789abcdef123456789abcdef123456789abcdef123456789abcdef1234";

#[derive(Parser)]
struct Cli {
    #[arg(short, long, default_value = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2")]
    sell_token: String,
    #[arg(short, long, default_value = "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48")]
    buy_token: String,
    #[arg(short, long, default_value_t = 1.0)]
    sell_amount: f64,
    /// The tvl threshold to filter the graph by
    #[arg(short, long, default_value_t = 100.0)]
    tvl_threshold: f64,
    #[arg(short, long, default_value = FAKE_PK)]
    swapper_pk: String,
    #[arg(short, long, default_value = "ethereum")]
    chain: String,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_target(false)
        .init();

    let cli = Cli::parse();
    let chain =
        Chain::from_str(&cli.chain).unwrap_or_else(|_| panic!("Unknown chain {}", cli.chain));

    let tycho_url = env::var("TYCHO_URL").unwrap_or_else(|_| {
        get_default_url(&chain).unwrap_or_else(|| panic!("Unknown URL for chain {}", cli.chain))
    });

    let tycho_api_key: String =
        env::var("TYCHO_API_KEY").unwrap_or_else(|_| "sampletoken".to_string());

    let tvl_filter = ComponentFilter::with_tvl_range(cli.tvl_threshold, cli.tvl_threshold);

    println!("Loading tokens from Tycho... {}", tycho_url.as_str());
    let all_tokens =
        load_all_tokens(tycho_url.as_str(), false, Some(tycho_api_key.as_str()), chain, None, None)
            .await;
    println!("Tokens loaded: {}", all_tokens.len());

    let sell_token_address =
        Bytes::from_str(&cli.sell_token).expect("Invalid address for sell token");
    let buy_token_address = Bytes::from_str(&cli.buy_token).expect("Invalid address for buy token");
    let sell_token = all_tokens
        .get(&sell_token_address)
        .expect("Sell token not found")
        .clone();
    let buy_token = all_tokens
        .get(&buy_token_address)
        .expect("Buy token not found")
        .clone();
    let amount_in =
        BigUint::from((cli.sell_amount * 10f64.powi(sell_token.decimals as i32)) as u128);

    println!(
        "Looking for pool with best price for {} {} -> {}",
        cli.sell_amount, sell_token.symbol, buy_token.symbol
    );
    let mut pairs: HashMap<String, ProtocolComponent> = HashMap::new();
    let mut amounts_out: HashMap<String, BigUint> = HashMap::new();

    let mut protocol_stream = ProtocolStreamBuilder::new(&tycho_url, chain);

    match chain {
        Chain::Ethereum => {
            protocol_stream = protocol_stream
                .exchange::<UniswapV2State>("uniswap_v2", tvl_filter.clone(), None)
                .exchange::<UniswapV2State>("sushiswap_v2", tvl_filter.clone(), None)
                .exchange::<UniswapV2State>("pancakeswap_v2", tvl_filter.clone(), None)
                .exchange::<UniswapV3State>("uniswap_v3", tvl_filter.clone(), None)
                .exchange::<UniswapV3State>("pancakeswap_v3", tvl_filter.clone(), None)
                .exchange::<EVMPoolState<PreCachedDB>>(
                    "vm:balancer_v2",
                    tvl_filter.clone(),
                    Some(balancer_pool_filter),
                )
                .exchange::<UniswapV4State>(
                    "uniswap_v4",
                    tvl_filter.clone(),
                    Some(uniswap_v4_pool_with_hook_filter),
                )
                .exchange::<EkuboState>("ekubo_v2", tvl_filter.clone(), None);
        }
        Chain::Base => {
            protocol_stream = protocol_stream
                .exchange::<UniswapV2State>("uniswap_v2", tvl_filter.clone(), None)
                .exchange::<UniswapV3State>("uniswap_v3", tvl_filter.clone(), None)
                .exchange::<UniswapV4State>(
                    "uniswap_v4",
                    tvl_filter.clone(),
                    Some(uniswap_v4_pool_with_hook_filter),
                )
        }
        Chain::Unichain => {
            protocol_stream = protocol_stream
                .exchange::<UniswapV2State>("uniswap_v2", tvl_filter.clone(), None)
                .exchange::<UniswapV3State>("uniswap_v3", tvl_filter.clone(), None)
                .exchange::<UniswapV4State>(
                    "uniswap_v4",
                    tvl_filter.clone(),
                    Some(uniswap_v4_pool_with_hook_filter),
                )
        }
        _ => {}
    }

    let mut protocol_stream = protocol_stream
        .auth_key(Some(tycho_api_key.clone()))
        .skip_state_decode_failures(true)
        .set_tokens(all_tokens.clone())
        .await
        .build()
        .await
        .expect("Failed building protocol stream");

    // Initialize the encoder
    let encoder = EVMEncoderBuilder::new()
        .chain(chain)
        .initialize_tycho_router_with_permit2(cli.swapper_pk.clone())
        .expect("Failed to create encoder builder")
        .build()
        .expect("Failed to build encoder");

    let wallet = PrivateKeySigner::from_bytes(
        &B256::from_str(&cli.swapper_pk).expect("Failed to convert swapper pk to B256"),
    )
    .expect("Failed to private key signer");
    let tx_signer = EthereumWallet::from(wallet.clone());
    let named_chain =
        NamedChain::from_str(&cli.chain.replace("ethereum", "mainnet")).expect("Invalid chain");
    let provider = ProviderBuilder::new()
        .with_chain(named_chain)
        .wallet(tx_signer.clone())
        .on_http(
            env::var("RPC_URL")
                .expect("RPC_URL env var not set")
                .parse()
                .expect("Failed to parse RPC_URL"),
        );

    while let Some(message_result) = protocol_stream.next().await {
        let message = match message_result {
            Ok(msg) => msg,
            Err(e) => {
                eprintln!("Error receiving message: {:?}. Continuing to next message...", e);
                continue;
            }
        };

        let best_swap = get_best_swap(
            message,
            &mut pairs,
            amount_in.clone(),
            sell_token.clone(),
            buy_token.clone(),
            &mut amounts_out,
        );

        if let Some((best_pool, expected_amount)) = best_swap {
            let component = pairs
                .get(&best_pool)
                .expect("Best pool not found")
                .clone();

            let tx = encode(
                encoder.clone(),
                component,
                sell_token.clone(),
                buy_token.clone(),
                amount_in.clone(),
                Bytes::from(wallet.address().to_vec()),
                expected_amount,
            );

            if cli.swapper_pk == FAKE_PK {
                println!("Signer private key was not provided. Skipping simulation/execution...");
                continue
            }
            println!("Would you like to simulate or execute this swap?");
            println!("Please be aware that the market might move while you make your decision, which might lead to a revert if you've set a min amount out or slippage.");
            println!("Warning: slippage is set to 0.25% during execution by default.");
            let options = vec!["Simulate the swap", "Execute the swap", "Skip this swap"];
            let selection = Select::with_theme(&ColorfulTheme::default())
                .with_prompt("What would you like to do?")
                .default(0)
                .items(&options)
                .interact()
                .unwrap_or(2); // Default to skip if error

            let choice = match selection {
                0 => "simulate",
                1 => "execute",
                _ => "skip",
            };

            match choice {
                "simulate" => {
                    println!("Simulating by performing an approval (for permit2) and a swap transaction...");

                    let (approval_request, swap_request) = get_tx_requests(
                        provider.clone(),
                        biguint_to_u256(&amount_in),
                        wallet.address(),
                        Address::from_slice(&sell_token_address),
                        tx.clone(),
                        named_chain as u64,
                    )
                    .await;

                    let payload = SimulatePayload {
                        block_state_calls: vec![SimBlock {
                            block_overrides: None,
                            state_overrides: None,
                            calls: vec![approval_request, swap_request],
                        }],
                        trace_transfers: true,
                        validation: true,
                        return_full_transactions: true,
                    };

                    match provider.simulate(&payload).await {
                        Ok(output) => {
                            for block in output.iter() {
                                println!("Simulated Block {}:", block.inner.header.number);
                                for (j, transaction) in block.calls.iter().enumerate() {
                                    println!(
                                        "  Transaction {}: Status: {:?}, Gas Used: {}",
                                        j + 1,
                                        transaction.status,
                                        transaction.gas_used
                                    );
                                }
                            }
                        }
                        Err(e) => {
                            eprintln!("Simulation failed: {:?}", e);
                            println!("Your RPC provider does not support transaction simulation.");
                            println!("Do you want to proceed with execution instead?");
                            let yes_no_options = vec!["Yes", "No"];
                            let yes_no_selection = Select::with_theme(&ColorfulTheme::default())
                                .with_prompt("Do you want to proceed with execution instead?")
                                .default(1) // Default to No
                                .items(&yes_no_options)
                                .interact()
                                .unwrap_or(1); // Default to No if error

                            if yes_no_selection == 0 {
                                match execute_swap_transaction(
                                    provider.clone(),
                                    &amount_in,
                                    wallet.address(),
                                    &sell_token_address,
                                    tx.clone(),
                                    named_chain as u64,
                                )
                                .await
                                {
                                    Ok(_) => return,
                                    Err(e) => {
                                        eprintln!("Failed to execute transaction: {:?}", e);
                                        continue;
                                    }
                                }
                            } else {
                                println!("Skipping this swap...");
                                continue;
                            }
                        }
                    }
                    // println!("Full simulation logs: {:?}", output);
                    return;
                }
                "execute" => {
                    match execute_swap_transaction(
                        provider.clone(),
                        &amount_in,
                        wallet.address(),
                        &sell_token_address,
                        tx,
                        named_chain as u64,
                    )
                    .await
                    {
                        Ok(_) => return,
                        Err(e) => {
                            eprintln!("Failed to execute transaction: {:?}", e);
                            continue;
                        }
                    }
                }
                "skip" => {
                    println!("Skipping this swap...");
                    continue;
                }
                _ => {
                    println!("Invalid input. Please choose 'simulate', 'execute' or 'skip'.");
                    continue;
                }
            }
        }
    }
}

fn get_best_swap(
    message: BlockUpdate,
    pairs: &mut HashMap<String, ProtocolComponent>,
    amount_in: BigUint,
    sell_token: Token,
    buy_token: Token,
    amounts_out: &mut HashMap<String, BigUint>,
) -> Option<(String, BigUint)> {
    println!("==================== Received block {:?} ====================", message.block_number);
    for (id, comp) in message.new_pairs.iter() {
        pairs
            .entry(id.clone())
            .or_insert_with(|| comp.clone());
    }
    if message.states.is_empty() {
        println!("No pools of interest were updated this block. The best swap is the previous one");
        return None;
    }
    for (id, state) in message.states.iter() {
        if let Some(component) = pairs.get(id) {
            let tokens = &component.tokens;
            if HashSet::from([&sell_token, &buy_token]) == HashSet::from([&tokens[0], &tokens[1]]) {
                let amount_out = state
                    .get_amount_out(amount_in.clone(), &sell_token, &buy_token)
                    .map_err(|e| {
                        eprintln!("Error calculating amount out for Pool {:?}: {:?}", id, e)
                    })
                    .ok();
                if let Some(amount_out) = amount_out {
                    amounts_out.insert(id.clone(), amount_out.amount);
                }
                // If you would like to save spot prices instead of the amount out, do
                // let spot_price = state
                //     .spot_price(&tokens[0], &tokens[1])
                //     .ok();
            }
        }
    }
    if let Some((key, amount_out)) = amounts_out
        .iter()
        .max_by_key(|(_, value)| value.to_owned())
    {
        println!("The best swap (out of {} possible pools) is:", amounts_out.len());
        println!(
            "Protocol: {:?}",
            pairs
                .get(key)
                .expect("Failed to get best pool")
                .protocol_system
        );
        println!("Pool address: {:?}", key);
        let formatted_in = format_token_amount(&amount_in, &sell_token);
        let formatted_out = format_token_amount(amount_out, &buy_token);
        let (forward_price, reverse_price) =
            format_price_ratios(&amount_in, amount_out, &sell_token, &buy_token);

        println!(
            "Swap: {} {} -> {} {} \nPrice: {:.6} {} per {}, {:.6} {} per {}",
            formatted_in,
            sell_token.symbol,
            formatted_out,
            buy_token.symbol,
            forward_price,
            buy_token.symbol,
            sell_token.symbol,
            reverse_price,
            sell_token.symbol,
            buy_token.symbol
        );
        Some((key.to_string(), amount_out.clone()))
    } else {
        println!("There aren't pools with the tokens we are looking for");
        None
    }
}

#[allow(clippy::too_many_arguments)]
fn encode(
    encoder: EVMTychoEncoder,
    component: ProtocolComponent,
    sell_token: Token,
    buy_token: Token,
    sell_amount: BigUint,
    user_address: Bytes,
    expected_amount: BigUint,
) -> Transaction {
    // Prepare data to encode. First we need to create a swap object
    let simple_swap = Swap::new(
        component,
        sell_token.address.clone(),
        buy_token.address.clone(),
        // Split defines the fraction of the amount to be swapped. A value of 0 indicates 100% of
        // the amount or the total remaining balance.
        0f64,
    );

    // Then we create a solution object with the previous swap
    let solution = Solution {
        sender: user_address.clone(),
        receiver: user_address,
        given_token: sell_token.address,
        given_amount: sell_amount,
        checked_token: buy_token.address,
        slippage: Some(0.0025), // 0.25% slippage
        expected_amount: Some(expected_amount),
        exact_out: false,     // it's an exact in solution
        checked_amount: None, // the amount out will not be checked in execution
        swaps: vec![simple_swap],
        ..Default::default()
    };

    // Encode the solution
    encoder
        .encode_router_calldata(vec![solution.clone()])
        .expect("Failed to encode router calldata")[0]
        .clone()
}

async fn get_tx_requests(
    provider: FillProvider<
        JoinFill<Identity, WalletFiller<EthereumWallet>>,
        ReqwestProvider,
        Http<Client>,
        Ethereum,
    >,
    amount_in: U256,
    user_address: Address,
    sell_token_address: Address,
    tx: Transaction,
    chain_id: u64,
) -> (TransactionRequest, TransactionRequest) {
    let block = provider
        .get_block_by_number(BlockNumberOrTag::Latest, false)
        .await
        .expect("Failed to fetch latest block")
        .expect("Block not found");

    let base_fee = block
        .header
        .base_fee_per_gas
        .expect("Base fee not available");
    let max_priority_fee_per_gas = 1_000_000_000u64;
    let max_fee_per_gas = base_fee + max_priority_fee_per_gas;

    let approve_function_signature = "approve(address,uint256)";
    let args = (
        Address::from_str("0x000000000022D473030F116dDEE9F6B43aC78BA3")
            .expect("Couldn't convert to address"),
        amount_in,
    );
    let data = encode_input(approve_function_signature, args.abi_encode());
    let nonce = provider
        .get_transaction_count(user_address)
        .await
        .expect("Failed to get nonce");

    let approval_request = TransactionRequest {
        to: Some(TxKind::Call(sell_token_address)),
        from: Some(user_address),
        value: None,
        input: TransactionInput { input: Some(AlloyBytes::from(data)), data: None },
        gas: Some(100_000u64),
        chain_id: Some(chain_id),
        max_fee_per_gas: Some(max_fee_per_gas.into()),
        max_priority_fee_per_gas: Some(max_priority_fee_per_gas.into()),
        nonce: Some(nonce),
        ..Default::default()
    };

    let swap_request = TransactionRequest {
        to: Some(TxKind::Call(Address::from_slice(&tx.to))),
        from: Some(user_address),
        value: Some(biguint_to_u256(&tx.value)),
        input: TransactionInput { input: Some(AlloyBytes::from(tx.data)), data: None },
        gas: Some(300_000u64),
        chain_id: Some(chain_id),
        max_fee_per_gas: Some(max_fee_per_gas.into()),
        max_priority_fee_per_gas: Some(max_priority_fee_per_gas.into()),
        nonce: Some(nonce + 1),
        ..Default::default()
    };
    (approval_request, swap_request)
}

// Format token amounts to human-readable values
fn format_token_amount(amount: &BigUint, token: &Token) -> String {
    let decimal_amount = amount.to_f64().unwrap_or(0.0) / 10f64.powi(token.decimals as i32);
    format!("{:.6}", decimal_amount)
}

// Calculate price ratios in both directions
fn format_price_ratios(
    amount_in: &BigUint,
    amount_out: &BigUint,
    token_in: &Token,
    token_out: &Token,
) -> (f64, f64) {
    let decimal_in = amount_in.to_f64().unwrap_or(0.0) / 10f64.powi(token_in.decimals as i32);
    let decimal_out = amount_out.to_f64().unwrap_or(0.0) / 10f64.powi(token_out.decimals as i32);

    if decimal_in > 0.0 && decimal_out > 0.0 {
        let forward = decimal_out / decimal_in;
        let reverse = decimal_in / decimal_out;
        (forward, reverse)
    } else {
        (0.0, 0.0)
    }
}

async fn execute_swap_transaction(
    provider: FillProvider<
        JoinFill<Identity, WalletFiller<EthereumWallet>>,
        ReqwestProvider,
        Http<Client>,
        Ethereum,
    >,
    amount_in: &BigUint,
    wallet_address: Address,
    sell_token_address: &Bytes,
    tx: Transaction,
    chain_id: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    println!("Executing by performing an approval (for permit2) and a swap transaction...");
    let (approval_request, swap_request) = get_tx_requests(
        provider.clone(),
        biguint_to_u256(amount_in),
        wallet_address,
        Address::from_slice(sell_token_address),
        tx.clone(),
        chain_id,
    )
    .await;

    let approval_receipt = provider
        .send_transaction(approval_request)
        .await?;

    let approval_result = approval_receipt.get_receipt().await?;
    println!(
        "Approval transaction sent with hash: {:?} and status: {:?}",
        approval_result.transaction_hash,
        approval_result.status()
    );

    let swap_receipt = provider
        .send_transaction(swap_request)
        .await?;

    let swap_result = swap_receipt.get_receipt().await?;
    println!(
        "Swap transaction sent with hash: {:?} and status: {:?}",
        swap_result.transaction_hash,
        swap_result.status()
    );

    Ok(())
}
