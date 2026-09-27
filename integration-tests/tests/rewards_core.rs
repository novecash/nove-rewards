use anchor_lang::prelude::*;
use anchor_litesvm::{AnchorContext, AnchorLiteSVM, TestHelpers};
use nove_rewards::{CLAIM_WINDOW_SECONDS, ID};
use solana_signer::Signer;

const PROGRAM_BYTES: &[u8] = include_bytes!("../../target/deploy/nove_rewards.so");
const USD: u64 = 1_000_000; // 6-decimal stablecoin

struct Setup {
    ctx: AnchorContext,
    admin: solana_keypair::Keypair,
    operator: solana_keypair::Keypair,
    mint: Pubkey,
    config: Pubkey,
    vault_authority: Pubkey,
    vault: Pubkey,
}

fn alloc_pda(wallet: &Pubkey, rank_id: u8) -> Pubkey {
    Pubkey::find_program_address(&[b"alloc", wallet.as_ref(), &[rank_id]], &ID).0
}

fn setup() -> Setup {
    let mut ctx = AnchorLiteSVM::build_with_program(ID, PROGRAM_BYTES);
    let admin = ctx.svm.create_funded_account(20_000_000_000).expect("admin");
    let operator = ctx.svm.create_funded_account(20_000_000_000).expect("operator");
    let mint_kp = ctx.svm.create_token_mint(&admin, 6).expect("mint");
    let mint = mint_kp.pubkey();

    let (config, _) = Pubkey::find_program_address(&[b"config"], &ID);
    let (vault_authority, _) = Pubkey::find_program_address(&[b"vault-authority"], &ID);
    let (vault, _) = Pubkey::find_program_address(&[b"vault", mint.as_ref()], &ID);

    let ix = ctx
        .program()
        .accounts(nove_rewards::accounts::Initialize {
            admin: admin.pubkey(),
            operator: operator.pubkey(),
            mint,
            config,
            vault_authority,
            vault,
            token_program: anchor_spl::token::ID,
            system_program: anchor_lang::system_program::ID,
        })
        .args(nove_rewards::instruction::Initialize {})
        .instruction()
        .expect("init ix");
    ctx.execute_instruction(ix, &[&admin]).expect("init tx").assert_success();

    // Fund the vault with the monthly load (mint authority is `admin` helper).
    ctx.svm
        .mint_to(&mint, &vault, &admin, 100_000 * USD)
        .expect("fund vault");

    Setup { ctx, admin, operator, mint, config, vault_authority, vault }
}

fn allocate_ix(
    s: &mut Setup,
    beneficiary: Pubkey,
    rank_id: u8,
    amount: u64,
    opens_at: i64,
) -> solana_instruction::Instruction {
    s.ctx
        .program()
        .accounts(nove_rewards::accounts::Allocate {
            operator: s.operator.pubkey(),
            beneficiary,
            config: s.config,
            allocation: alloc_pda(&beneficiary, rank_id),
            system_program: anchor_lang::system_program::ID,
        })
        .args(nove_rewards::instruction::Allocate { rank_id, amount, opens_at })
        .instruction()
        .expect("allocate ix")
}

fn claim_ix(
    s: &mut Setup,
    wallet: Pubkey,
    rank_id: u8,
    destination: Pubkey,
) -> solana_instruction::Instruction {
    s.ctx
        .program()
        .accounts(nove_rewards::accounts::Claim {
            wallet,
            config: s.config,
            allocation: alloc_pda(&wallet, rank_id),
            vault_authority: s.vault_authority,
            vault: s.vault,
            destination,
            token_program: anchor_spl::token::ID,
        })
        .args(nove_rewards::instruction::Claim {})
        .instruction()
        .expect("claim ix")
}

fn warp_to(s: &mut Setup, ts: i64) {
    let mut clock: Clock = s.ctx.svm.get_sysvar();
    clock.unix_timestamp = ts;
    s.ctx.svm.set_sysvar(&clock);
}

fn token_balance(s: &Setup, account: &Pubkey) -> u64 {
    let data = s.ctx.svm.get_account(account).expect("token account").data;
    u64::from_le_bytes(data[64..72].try_into().unwrap())
}

#[test]
fn happy_path_allocate_then_claim_inside_window() {
    let mut s = setup();
    let member = s.ctx.svm.create_funded_account(5_000_000_000).expect("member");
    let member_ata = s
        .ctx
        .svm
        .create_associated_token_account(&s.mint, &member)
        .expect("member ata");

    let now = s.ctx.svm.get_sysvar::<Clock>().unix_timestamp;
    let opens_at = now + 3600;
    let ix = allocate_ix(&mut s, member.pubkey(), 3, 250 * USD, opens_at);
    s.ctx.execute_instruction(ix, &[&s.operator.insecure_clone()]).expect("alloc tx").assert_success();

    warp_to(&mut s, opens_at + 60);
    let ix = claim_ix(&mut s, member.pubkey(), 3, member_ata);
    s.ctx.execute_instruction(ix, &[&member]).expect("claim tx").assert_success();

    assert_eq!(token_balance(&s, &member_ata), 250 * USD, "reward paid in full");

    let alloc = s.ctx.svm.get_account(&alloc_pda(&member.pubkey(), 3)).expect("alloc account");
    // claimed flag lives right after wallet(32)+rank(1)+amount(8)+opens_at(8) = offset 8+49
    assert_eq!(alloc.data[8 + 49], 1, "allocation tombstoned as claimed");
}

#[test]
fn second_claim_is_rejected() {
    let mut s = setup();
    let member = s.ctx.svm.create_funded_account(5_000_000_000).expect("member");
    let member_ata = s.ctx.svm.create_associated_token_account(&s.mint, &member).expect("ata");
    let now = s.ctx.svm.get_sysvar::<Clock>().unix_timestamp;
    let ix = allocate_ix(&mut s, member.pubkey(), 3, 250 * USD, now + 10);
    s.ctx.execute_instruction(ix, &[&s.operator.insecure_clone()]).expect("alloc").assert_success();
    warp_to(&mut s, now + 20);
    let ix = claim_ix(&mut s, member.pubkey(), 3, member_ata);
    s.ctx.execute_instruction(ix, &[&member]).expect("claim1").assert_success();

    s.ctx.svm.expire_blockhash();
    let ix = claim_ix(&mut s, member.pubkey(), 3, member_ata);
    let second = s.ctx.execute_instruction(ix, &[&member]).expect("claim2 executed");
    assert!(!second.is_success(), "double claim must fail");
    assert_eq!(token_balance(&s, &member_ata), 250 * USD, "no double payout");
}

#[test]
fn claim_before_open_and_after_window_are_rejected() {
    let mut s = setup();
    let member = s.ctx.svm.create_funded_account(5_000_000_000).expect("member");
    let member_ata = s.ctx.svm.create_associated_token_account(&s.mint, &member).expect("ata");
    let now = s.ctx.svm.get_sysvar::<Clock>().unix_timestamp;
    let opens_at = now + 86_400;
    let ix = allocate_ix(&mut s, member.pubkey(), 4, 750 * USD, opens_at);
    s.ctx.execute_instruction(ix, &[&s.operator.insecure_clone()]).expect("alloc").assert_success();

    // too early
    let ix = claim_ix(&mut s, member.pubkey(), 4, member_ata);
    let early = s.ctx.execute_instruction(ix, &[&member]).expect("early exec");
    assert!(!early.is_success(), "claim before opens_at must fail");

    // too late: past the 90-day window
    warp_to(&mut s, opens_at + CLAIM_WINDOW_SECONDS + 1);
    s.ctx.svm.expire_blockhash();
    let ix = claim_ix(&mut s, member.pubkey(), 4, member_ata);
    let late = s.ctx.execute_instruction(ix, &[&member]).expect("late exec");
    assert!(!late.is_success(), "claim after the 90-day window must fail");
    assert_eq!(token_balance(&s, &member_ata), 0, "nothing paid outside the window");
}

#[test]
fn duplicate_allocation_for_same_rank_is_rejected() {
    let mut s = setup();
    let member = s.ctx.svm.create_funded_account(5_000_000_000).expect("member");
    let now = s.ctx.svm.get_sysvar::<Clock>().unix_timestamp;
    let ix = allocate_ix(&mut s, member.pubkey(), 5, 2_500 * USD, now + 10);
    s.ctx.execute_instruction(ix, &[&s.operator.insecure_clone()]).expect("alloc1").assert_success();

    s.ctx.svm.expire_blockhash();
    let ix = allocate_ix(&mut s, member.pubkey(), 5, 2_500 * USD, now + 10);
    let dup = s.ctx.execute_instruction(ix, &[&s.operator.insecure_clone()]).expect("alloc2 exec");
    assert!(!dup.is_success(), "one-shot per wallet+rank: duplicate allocation must fail");
}

#[test]
fn only_operator_can_allocate() {
    let mut s = setup();
    let stranger = s.ctx.svm.create_funded_account(5_000_000_000).expect("stranger");
    let member = s.ctx.svm.create_funded_account(5_000_000_000).expect("member");
    let now = s.ctx.svm.get_sysvar::<Clock>().unix_timestamp;

    let ix = s
        .ctx
        .program()
        .accounts(nove_rewards::accounts::Allocate {
            operator: stranger.pubkey(), // not the configured operator
            beneficiary: member.pubkey(),
            config: s.config,
            allocation: alloc_pda(&member.pubkey(), 2),
            system_program: anchor_lang::system_program::ID,
        })
        .args(nove_rewards::instruction::Allocate { rank_id: 2, amount: USD, opens_at: now })
        .instruction()
        .expect("ix");
    let res = s.ctx.execute_instruction(ix, &[&stranger]).expect("exec");
    assert!(!res.is_success(), "non-operator allocation must fail");
}

#[test]
fn another_wallet_cannot_claim_someone_elses_reward() {
    let mut s = setup();
    let member = s.ctx.svm.create_funded_account(5_000_000_000).expect("member");
    let thief = s.ctx.svm.create_funded_account(5_000_000_000).expect("thief");
    let thief_ata = s.ctx.svm.create_associated_token_account(&s.mint, &thief).expect("ata");
    let now = s.ctx.svm.get_sysvar::<Clock>().unix_timestamp;
    let ix = allocate_ix(&mut s, member.pubkey(), 6, 7_500 * USD, now);
    s.ctx.execute_instruction(ix, &[&s.operator.insecure_clone()]).expect("alloc").assert_success();

    // Thief signs; the allocation PDA is derived from the thief's key, so it
    // points at a non-existent account and the claim must fail.
    let ix = s
        .ctx
        .program()
        .accounts(nove_rewards::accounts::Claim {
            wallet: thief.pubkey(),
            config: s.config,
            allocation: alloc_pda(&member.pubkey(), 6), // explicit mismatch attempt
            vault_authority: s.vault_authority,
            vault: s.vault,
            destination: thief_ata,
            token_program: anchor_spl::token::ID,
        })
        .args(nove_rewards::instruction::Claim {})
        .instruction()
        .expect("ix");
    let res = s.ctx.execute_instruction(ix, &[&thief]).expect("exec");
    assert!(!res.is_success(), "cross-wallet claim must fail (seed mismatch)");
    assert_eq!(token_balance(&s, &thief_ata), 0);
}

#[test]
fn admin_withdraw_gated_to_admin() {
    let mut s = setup();
    let stranger = s.ctx.svm.create_funded_account(5_000_000_000).expect("stranger");
    let sink = s.ctx.svm.create_associated_token_account(&s.mint, &stranger).expect("sink");

    // stranger cannot drain
    let ix = s
        .ctx
        .program()
        .accounts(nove_rewards::accounts::AdminWithdraw {
            admin: stranger.pubkey(),
            config: s.config,
            vault_authority: s.vault_authority,
            vault: s.vault,
            destination: sink,
            token_program: anchor_spl::token::ID,
        })
        .args(nove_rewards::instruction::AdminWithdraw { amount: USD })
        .instruction()
        .expect("ix");
    let res = s.ctx.execute_instruction(ix, &[&stranger]).expect("exec");
    assert!(!res.is_success(), "non-admin withdraw must fail");

    // admin can (the unpublished emergency valve)
    s.ctx.svm.expire_blockhash();
    let admin = s.admin.insecure_clone();
    let admin_sink = s.ctx.svm.create_associated_token_account(&s.mint, &admin).expect("admin sink");
    let ix = s
        .ctx
        .program()
        .accounts(nove_rewards::accounts::AdminWithdraw {
            admin: admin.pubkey(),
            config: s.config,
            vault_authority: s.vault_authority,
            vault: s.vault,
            destination: admin_sink,
            token_program: anchor_spl::token::ID,
        })
        .args(nove_rewards::instruction::AdminWithdraw { amount: 5 * USD })
        .instruction()
        .expect("ix");
    s.ctx.execute_instruction(ix, &[&admin]).expect("exec").assert_success();
    assert_eq!(token_balance(&s, &admin_sink), 5 * USD);
}

#[test]
fn claim_fails_softly_when_vault_underfunded() {
    let mut s = setup();
    // Drain the vault first via admin (simulate a not-yet-loaded month).
    let admin = s.admin.insecure_clone();
    let sink = s.ctx.svm.create_associated_token_account(&s.mint, &admin).expect("sink");
    let vault_balance = token_balance(&s, &s.vault.clone());
    let ix = s
        .ctx
        .program()
        .accounts(nove_rewards::accounts::AdminWithdraw {
            admin: admin.pubkey(),
            config: s.config,
            vault_authority: s.vault_authority,
            vault: s.vault,
            destination: sink,
            token_program: anchor_spl::token::ID,
        })
        .args(nove_rewards::instruction::AdminWithdraw { amount: vault_balance })
        .instruction()
        .expect("ix");
    s.ctx.execute_instruction(ix, &[&admin]).expect("drain").assert_success();

    let member = s.ctx.svm.create_funded_account(5_000_000_000).expect("member");
    let member_ata = s.ctx.svm.create_associated_token_account(&s.mint, &member).expect("ata");
    let now = s.ctx.svm.get_sysvar::<Clock>().unix_timestamp;
    let ix = allocate_ix(&mut s, member.pubkey(), 3, 250 * USD, now);
    s.ctx.execute_instruction(ix, &[&s.operator.insecure_clone()]).expect("alloc").assert_success();

    let ix = claim_ix(&mut s, member.pubkey(), 3, member_ata);
    let res = s.ctx.execute_instruction(ix, &[&member]).expect("exec");
    assert!(!res.is_success(), "claim with empty vault must fail, not mint from thin air");

    // After a reload the same claim succeeds — pay-only-from-balance rule.
    s.ctx.svm.mint_to(&s.mint.clone(), &s.vault.clone(), &admin, 1_000 * USD).expect("reload");
    s.ctx.svm.expire_blockhash();
    let ix = claim_ix(&mut s, member.pubkey(), 3, member_ata);
    s.ctx.execute_instruction(ix, &[&member]).expect("exec2").assert_success();
    assert_eq!(token_balance(&s, &member_ata), 250 * USD);
}
