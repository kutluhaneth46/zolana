#[test]
#[ignore = "requires built localnet programs, CLI, Photon, and prover with proving keys"]
fn test_transaction_submission() -> anyhow::Result<()> {
    use anyhow::anyhow;
    use client_example::{setup, SetupContext};
    use solana_keypair::Keypair;
    use solana_signer::Signer;
    use zolana_client::{
        sign_transaction, IndexerRpcConfig, Rpc, SignedPrivateTransaction, SolanaRpc, Submission,
        UtxoScan, ZolanaClient,
    };
    use zolana_interface::pda;
    use zolana_program::instruction::{
        AssetDeposit, Deposit, DepositAsset, TransactInterfaceTransferAccounts,
        TransactSolTransferAccounts,
    };
    use zolana_transaction::{
        instructions::transact::{ConfidentialTransaction, Shape},
        AssetRegistry, SOL_MINT,
    };

    // Setup: start localnet and fund one private SOL note.
    let SetupContext {
        rpc_url,
        indexer_url,
        prover_url,
        tree_id,
        sender,
        recipient_address,
    } = setup()?;
    let client = ZolanaClient::from_urls(SolanaRpc::new(rpc_url), indexer_url, prover_url)?;
    let assets = AssetRegistry::default();
    let sender_address = sender.shielded_address()?;
    let withdrawal_recipient = Keypair::new().pubkey();
    let deposit_amount = 1_000_000_000;
    let transfer_amount = 300_000_000;
    let withdrawal_amount = 200_000_000;
    let deposit = Deposit {
        tree: pda::tree(tree_id),
        depositor: sender.pubkey(),
        deposits: vec![AssetDeposit {
            asset: DepositAsset::Sol,
            view_tag: sender_address.confidential_view_tag()?,
            owner: sender_address.owner_hash()?,
            amount: deposit_amount,
            memo: None,
        }],
    }
    .instruction()?;
    let deposit_signature = client.create_and_send_transaction(
        &[deposit],
        sender.pubkey(),
        &[&sender],
        client.compute_budget(),
    )?;
    client.confirm_private_transaction_sync(deposit_signature)?;
    let scan = UtxoScan::new(&sender, &assets)
        .with_indexer_config(IndexerRpcConfig::at_slot(client.get_slot()?))
        .fetch(&client)?;
    let balance = scan
        .spendable
        .balances
        .get_balance(SOL_MINT)
        .ok_or_else(|| anyhow!("the deposited SOL balance was not indexed"))?;
    let input = balance
        .utxos
        .iter()
        .find(|utxo| utxo.utxo.amount == deposit_amount)
        .cloned()
        .ok_or_else(|| anyhow!("the deposited SOL note was not indexed"))?;

    // 1. Build a private transfer and public withdrawal with private change.
    let mut transaction =
        ConfidentialTransaction::new(vec![input], sender.pubkey())?.with_output_tree_id(tree_id)?;
    transaction.transfer_sol(&recipient_address, transfer_amount)?;
    transaction.withdraw_sol(withdrawal_amount, withdrawal_recipient)?;
    transaction.pad_utxos(Shape::IN2_OUT4, &sender_address)?;

    // 2. Encrypt the outputs into proof inputs.
    let proof_inputs = transaction.encrypt(&sender)?;

    // 3. Include the public recipient's settlement account.
    let signed = SignedPrivateTransaction {
        transaction: proof_inputs,
        settlement_transfers: vec![TransactInterfaceTransferAccounts::Sol(
            TransactSolTransferAccounts {
                recipient: withdrawal_recipient,
            },
        )],
    };

    // 4. Describe who pays and who completes the proof inputs.
    let submission = Submission::new(&signed, sender.pubkey(), &sender);

    // 5–7. Fetch proofs, prove and verify, then build the unsigned Solana message.
    let unsigned = submission.finish_unsigned_sync(&client)?;

    // 8. Sign the message before its blockhash expires.
    assert!(client.get_block_height()? <= unsigned.last_valid_block_height);
    let transaction = sign_transaction(unsigned.message, &[&sender])?;

    // 9. Submit the signed Solana transaction.
    let signature = client.process_transaction(transaction)?;

    // 10. Wait for confirmation and indexing before reading the resulting notes.
    client.confirm_private_transaction_sync(signature)?;
    let scan = UtxoScan::new(&sender, &assets)
        .with_indexer_config(IndexerRpcConfig::at_slot(client.get_slot()?))
        .fetch(&client)?;
    let balance = scan
        .spendable
        .balances
        .get_balance(SOL_MINT)
        .ok_or_else(|| anyhow!("the private change was not indexed"))?;
    assert_eq!(
        (
            balance.amount,
            balance.utxos.len(),
            client.get_balance(withdrawal_recipient)?,
        ),
        (
            deposit_amount - transfer_amount - withdrawal_amount,
            1,
            withdrawal_amount,
        ),
        "the transaction must preserve private change and settle the public withdrawal",
    );
    Ok(())
}
