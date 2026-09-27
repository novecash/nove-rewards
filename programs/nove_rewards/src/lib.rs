//! NOVE rank rewards vault.
//!
//! Application-layer companion to the frozen Service Referral Protocol: holds
//! the promotional reward fund (2% of volume, loaded manually each month) and
//! lets members CLAIM their one-shot rank rewards.
//!
//! Design principles:
//! - Funds never move on allocation: the vault holds the pool; an allocation
//!   is only bookkeeping. An unclaimed (expired) reward simply stays in the
//!   vault for future months — no reclaim instruction, no extra surface.
//! - One-shot per wallet per rank is enforced by the allocation PDA seeds
//!   ([b"alloc", wallet, rank_id]) and by never closing claimed allocations:
//!   the account remains as a permanent tombstone.
//! - `operator` (the off-chain snapshot job) writes allocations; `admin`
//!   (cold key on the owner's machine) can rotate keys and, as an unpublished
//!   safety valve, withdraw the fund in an emergency.

use anchor_lang::prelude::*;
use anchor_spl::token::{self, Mint, Token, TokenAccount, Transfer};

declare_id!("68uo9JyYTgMv965BDRewPr2zp8eZypdnS5bfDnqm9m49");

pub const CLAIM_WINDOW_SECONDS: i64 = 90 * 24 * 60 * 60;

#[program]
pub mod nove_rewards {
    use super::*;

    pub fn initialize(ctx: Context<Initialize>) -> Result<()> {
        let cfg = &mut ctx.accounts.config;
        cfg.admin = ctx.accounts.admin.key();
        cfg.operator = ctx.accounts.operator.key();
        cfg.mint = ctx.accounts.mint.key();
        cfg.vault = ctx.accounts.vault.key();
        cfg.bump = ctx.bumps.config;
        cfg.vault_authority_bump = ctx.bumps.vault_authority;
        Ok(())
    }

    /// Operator books one wallet's reward for one rank. `opens_at` is the
    /// first second the member may claim (the 1st of the following month);
    /// the window closes `CLAIM_WINDOW_SECONDS` later.
    pub fn allocate(
        ctx: Context<Allocate>,
        rank_id: u8,
        amount: u64,
        opens_at: i64,
    ) -> Result<()> {
        require!(amount > 0, RewardsError::ZeroAmount);
        require!(rank_id <= 8, RewardsError::InvalidRank);
        let alloc = &mut ctx.accounts.allocation;
        alloc.wallet = ctx.accounts.beneficiary.key();
        alloc.rank_id = rank_id;
        alloc.amount = amount;
        alloc.opens_at = opens_at;
        alloc.claimed = false;
        alloc.claimed_at = 0;
        alloc.bump = ctx.bumps.allocation;
        Ok(())
    }

    /// The member pulls their reward. Only inside the claim window, only
    /// once — the allocation account stays forever as the paid receipt.
    pub fn claim(ctx: Context<Claim>) -> Result<()> {
        let now = Clock::get()?.unix_timestamp;
        let alloc = &mut ctx.accounts.allocation;
        require!(!alloc.claimed, RewardsError::AlreadyClaimed);
        require!(now >= alloc.opens_at, RewardsError::NotOpenYet);
        require!(
            now <= alloc
                .opens_at
                .checked_add(CLAIM_WINDOW_SECONDS)
                .ok_or(RewardsError::Overflow)?,
            RewardsError::WindowClosed
        );

        alloc.claimed = true;
        alloc.claimed_at = now;

        let bump = [ctx.accounts.config.vault_authority_bump];
        let seeds: &[&[u8]] = &[b"vault-authority", &bump];
        token::transfer(
            CpiContext::new_with_signer(
                ctx.accounts.token_program.key(),
                Transfer {
                    from: ctx.accounts.vault.to_account_info(),
                    to: ctx.accounts.destination.to_account_info(),
                    authority: ctx.accounts.vault_authority.to_account_info(),
                },
                &[seeds],
            ),
            alloc.amount,
        )?;
        emit!(RewardClaimed {
            wallet: alloc.wallet,
            rank_id: alloc.rank_id,
            amount: alloc.amount,
        });
        Ok(())
    }

    /// Admin-only emergency drain (unpublished safety valve against vault
    /// compromise). Destination is free on purpose: emergencies are messy.
    pub fn admin_withdraw(ctx: Context<AdminWithdraw>, amount: u64) -> Result<()> {
        let bump = [ctx.accounts.config.vault_authority_bump];
        let seeds: &[&[u8]] = &[b"vault-authority", &bump];
        token::transfer(
            CpiContext::new_with_signer(
                ctx.accounts.token_program.key(),
                Transfer {
                    from: ctx.accounts.vault.to_account_info(),
                    to: ctx.accounts.destination.to_account_info(),
                    authority: ctx.accounts.vault_authority.to_account_info(),
                },
                &[seeds],
            ),
            amount,
        )?;
        Ok(())
    }

    pub fn set_operator(ctx: Context<AdminOnly>, new_operator: Pubkey) -> Result<()> {
        ctx.accounts.config.operator = new_operator;
        Ok(())
    }

    pub fn set_admin(ctx: Context<AdminOnly>, new_admin: Pubkey) -> Result<()> {
        ctx.accounts.config.admin = new_admin;
        Ok(())
    }
}

#[derive(Accounts)]
pub struct Initialize<'info> {
    #[account(mut)]
    pub admin: Signer<'info>,
    /// CHECK: recorded as the allocation-writer key; no data read.
    pub operator: UncheckedAccount<'info>,
    pub mint: Account<'info, Mint>,
    #[account(
        init,
        payer = admin,
        space = 8 + RewardsConfig::INIT_SPACE,
        seeds = [b"config"],
        bump
    )]
    pub config: Account<'info, RewardsConfig>,
    /// CHECK: PDA that owns the vault token account; never holds data.
    #[account(seeds = [b"vault-authority"], bump)]
    pub vault_authority: UncheckedAccount<'info>,
    #[account(
        init,
        payer = admin,
        token::mint = mint,
        token::authority = vault_authority,
        seeds = [b"vault", mint.key().as_ref()],
        bump
    )]
    pub vault: Account<'info, TokenAccount>,
    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
#[instruction(rank_id: u8)]
pub struct Allocate<'info> {
    #[account(mut, address = config.operator @ RewardsError::Unauthorized)]
    pub operator: Signer<'info>,
    /// CHECK: pure pubkey used as allocation seed / future claimer.
    pub beneficiary: UncheckedAccount<'info>,
    #[account(seeds = [b"config"], bump = config.bump)]
    pub config: Account<'info, RewardsConfig>,
    #[account(
        init,
        payer = operator,
        space = 8 + RewardAllocation::INIT_SPACE,
        seeds = [b"alloc", beneficiary.key().as_ref(), &[rank_id]],
        bump
    )]
    pub allocation: Account<'info, RewardAllocation>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct Claim<'info> {
    #[account(mut)]
    pub wallet: Signer<'info>,
    #[account(seeds = [b"config"], bump = config.bump)]
    pub config: Account<'info, RewardsConfig>,
    #[account(
        mut,
        seeds = [b"alloc", wallet.key().as_ref(), &[allocation.rank_id]],
        bump = allocation.bump,
        constraint = allocation.wallet == wallet.key() @ RewardsError::Unauthorized
    )]
    pub allocation: Account<'info, RewardAllocation>,
    /// CHECK: verified by seeds.
    #[account(seeds = [b"vault-authority"], bump = config.vault_authority_bump)]
    pub vault_authority: UncheckedAccount<'info>,
    #[account(mut, address = config.vault @ RewardsError::WrongVault)]
    pub vault: Account<'info, TokenAccount>,
    #[account(
        mut,
        token::mint = config.mint,
        token::authority = wallet
    )]
    pub destination: Account<'info, TokenAccount>,
    pub token_program: Program<'info, Token>,
}

#[derive(Accounts)]
pub struct AdminWithdraw<'info> {
    #[account(address = config.admin @ RewardsError::Unauthorized)]
    pub admin: Signer<'info>,
    #[account(seeds = [b"config"], bump = config.bump)]
    pub config: Account<'info, RewardsConfig>,
    /// CHECK: verified by seeds.
    #[account(seeds = [b"vault-authority"], bump = config.vault_authority_bump)]
    pub vault_authority: UncheckedAccount<'info>,
    #[account(mut, address = config.vault @ RewardsError::WrongVault)]
    pub vault: Account<'info, TokenAccount>,
    #[account(mut)]
    pub destination: Account<'info, TokenAccount>,
    pub token_program: Program<'info, Token>,
}

#[derive(Accounts)]
pub struct AdminOnly<'info> {
    #[account(address = config.admin @ RewardsError::Unauthorized)]
    pub admin: Signer<'info>,
    #[account(mut, seeds = [b"config"], bump = config.bump)]
    pub config: Account<'info, RewardsConfig>,
}

#[account]
#[derive(InitSpace)]
pub struct RewardsConfig {
    pub admin: Pubkey,
    pub operator: Pubkey,
    pub mint: Pubkey,
    pub vault: Pubkey,
    pub bump: u8,
    pub vault_authority_bump: u8,
}

#[account]
#[derive(InitSpace)]
pub struct RewardAllocation {
    pub wallet: Pubkey,
    pub rank_id: u8,
    pub amount: u64,
    pub opens_at: i64,
    pub claimed: bool,
    pub claimed_at: i64,
    pub bump: u8,
}

#[event]
pub struct RewardClaimed {
    pub wallet: Pubkey,
    pub rank_id: u8,
    pub amount: u64,
}

#[error_code]
pub enum RewardsError {
    #[msg("Unauthorized")]
    Unauthorized,
    #[msg("Amount must be greater than zero")]
    ZeroAmount,
    #[msg("Invalid rank id")]
    InvalidRank,
    #[msg("Reward already claimed")]
    AlreadyClaimed,
    #[msg("Claim window not open yet")]
    NotOpenYet,
    #[msg("Claim window closed")]
    WindowClosed,
    #[msg("Wrong vault account")]
    WrongVault,
    #[msg("Arithmetic overflow")]
    Overflow,
}
