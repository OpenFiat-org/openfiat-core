//! Why every mint in this workspace carries `mint::token_program`.
//!
//! All four programs used to declare their token program as
//! `Program<'info, Token2022>`, which made the CPI target a constant: there
//! was exactly one program a `transfer_checked` could be dispatched to, and
//! Anchor rejected anything else before a handler ran.
//!
//! That constant was wrong. A mint is owned by *one* token program, chosen
//! when the mint is created, and the assets this protocol actually settles in
//! are not all Token-2022. On devnet, wSOL
//! (`So11111111111111111111111111111111111111112`) and USDC
//! (`4zMMC9srt5Ri5X14GAgXhaHii3GnPAEERYPJgZJDncDU`) are both owned by the
//! legacy SPL Token program; only OPEN is Token-2022. So the escrow could not
//! hold a single one of the stablecoins it exists to escrow. Every test passed
//! because every fixture mint was created as Token-2022 — the fixtures had
//! been written to match the constraint rather than to match production, which
//! is why nothing ever surfaced it.
//!
//! # What replacing it costs, and what has to be paid back
//!
//! `Interface<'info, TokenInterface>` accepts either program. On its own that
//! is strictly *less* safe than what it replaces: the CPI target stops being a
//! constant and becomes caller-supplied, while `InterfaceAccount<Mint>` and
//! `InterfaceAccount<TokenAccount>` happily deserialize accounts from either
//! program. A caller could then hand in a legacy-SPL token program alongside a
//! Token-2022 mint, or the reverse, and the program would build a
//! `transfer_checked` aimed at a program that does not own the accounts.
//!
//! `#[account(mint::token_program = token_program)]` is what pays that back.
//! It compiles to `mint_account_info.owner == token_program.key()`, which is
//! precisely the invariant `Program<'info, Token2022>` used to provide for
//! free — the supplied program is the mint's owner — except derived from the
//! mint at runtime instead of fixed at compile time. Every mint that shares an
//! `Accounts` struct with a token program carries it, and adding a mint
//! without it silently re-opens the hole.
//!
//! The token *accounts* need no equivalent constraint. A token account can
//! only be initialized by the program that owns its mint — SPL Token's
//! `InitializeAccount` requires the mint to be owned by SPL Token, and
//! Token-2022's requires Token-2022 — so `token::mint = mint` already implies
//! the account's owning program, transitively, once the mint's owner is
//! pinned.
//!
//! # One token program per mint, not per instruction
//!
//! The corollary is easy to miss: an instruction that moves *two* mints needs
//! *two* program handles, because the two mints may be owned by different
//! programs. `escrow::execute_dispute_outcome` is the case in this workspace —
//! it settles a trade in the settlement stablecoin and an arbitration deposit
//! in OPEN, in one atomic call. Sharing one handle there would have left the
//! whole dispute-resolution path uncallable for exactly the
//! legacy-stablecoin/Token-2022-OPEN pairing this migration exists to support,
//! and it would have failed at CPI time rather than at account validation.
