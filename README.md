# Nove — rewards program

Source code of the rank-prize program of [Nove](https://nove.cash) on Solana mainnet. It is a companion to the protocol program ([novecash/nove-protocol](https://github.com/novecash/nove-protocol)) and never touches the protocol's 50/43/2/5 split.

| | |
|---|---|
| Program ID | `68uo9JyYTgMv965BDRewPr2zp8eZypdnS5bfDnqm9m49` |
| Network | Solana mainnet-beta |
| Deployed binary SHA-256 | `36de56bb303b1b22f134f598dbce07025539e7d79baef67311df59cc7794fb57` |

## What it does

Members who reach a rank from Platinum up can claim a one-time USDC prize: $250 (Platinum), $750 (Emerald), $2,500 (Diamond), $7,500 (Double Diamond), $25,000 (Crown), $75,000 (Crown Ambassador).

- The prize fund sits in a public vault owned by the program.
- An `operator` key records each allocation (wallet, rank, amount, opening time). Recording moves no funds.
- The member claims the prize to their own wallet; a claim window lasts 90 days.
- One prize per wallet per rank, forever: enforced by the allocation account seeds `["alloc", wallet, rank_id]` and by never closing claimed allocations.
- An `admin` key can rotate the operator and admin keys and withdraw from the vault. Prizes are therefore a promotional program: their availability depends on the vault's public balance.

Instructions: `initialize`, `allocate`, `claim`, `admin_withdraw`, `set_operator`, `set_admin`.

## Verify

```bash
solana program dump -u mainnet-beta 68uo9JyYTgMv965BDRewPr2zp8eZypdnS5bfDnqm9m49 rewards.so
sha256sum rewards.so
# 36de56bb303b1b22f134f598dbce07025539e7d79baef67311df59cc7794fb57

solana program show -u mainnet-beta 68uo9JyYTgMv965BDRewPr2zp8eZypdnS5bfDnqm9m49
# Authority: none   → the code can no longer change
```

## Tests

`integration-tests/` — LiteSVM tests of allocation, claim, the 90-day window, double-claim protection and key permissions.
