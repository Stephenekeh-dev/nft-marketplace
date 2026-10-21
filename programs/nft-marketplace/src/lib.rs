use anchor_lang::prelude::*;
use anchor_lang::system_program::{self, Transfer as SystemTransfer};
use anchor_spl::associated_token::AssociatedToken;
use anchor_spl::token::{self, CloseAccount, Mint, Token, TokenAccount, Transfer};

declare_id!("E6CZ4yrxREvTUUjHz2hD4vAATW2yhKoFUTi1MKrXXp9F");

/// Marketplace fee ceiling: 10%.
pub const MAX_FEE_BPS: u16 = 1_000;

#[program]
pub mod nft_marketplace {
    use super::*;

    /// One-time setup. Records the admin, the wallet that collects fees,
    /// and the fee in basis points (250 = 2.5%).
    pub fn initialize_marketplace(ctx: Context<InitializeMarketplace>, fee_bps: u16) -> Result<()> {
        require!(fee_bps <= MAX_FEE_BPS, MarketError::InvalidFee);

        let marketplace = &mut ctx.accounts.marketplace;
        marketplace.admin = ctx.accounts.admin.key();
        marketplace.treasury = ctx.accounts.treasury.key();
        marketplace.fee_bps = fee_bps;
        marketplace.bump = ctx.bumps.marketplace;
        Ok(())
    }

    /// Escrows the NFT in a token account owned by the listing PDA and records the price.
    pub fn list_nft(ctx: Context<ListNft>, price: u64) -> Result<()> {
        require!(price > 0, MarketError::InvalidPrice);

        // Only real NFTs: zero decimals, supply of exactly one, and no way to mint more.
        let mint = &ctx.accounts.mint;
        require!(mint.decimals == 0 && mint.supply == 1, MarketError::NotAnNft);
        require!(mint.mint_authority.is_none(), MarketError::MintAuthorityNotRevoked);
        require!(ctx.accounts.seller_token_account.amount == 1, MarketError::NftNotOwned);

        let cpi_accounts = Transfer {
            from: ctx.accounts.seller_token_account.to_account_info(),
            to: ctx.accounts.escrow_token_account.to_account_info(),
            authority: ctx.accounts.seller.to_account_info(),
        };
        token::transfer(CpiContext::new(ctx.accounts.token_program.key(), cpi_accounts), 1)?;

        let listing = &mut ctx.accounts.listing;
        listing.seller = ctx.accounts.seller.key();
        listing.mint = ctx.accounts.mint.key();
        listing.price = price;
        listing.bump = ctx.bumps.listing;

        emit!(Listed {
            seller: listing.seller,
            mint: listing.mint,
            price,
        });
        Ok(())
    }

    /// Atomic sale: SOL to the seller, fee to the treasury, NFT to the buyer.
    /// The listing PDA signs the NFT out of escrow.
    pub fn buy_nft(ctx: Context<BuyNft>) -> Result<()> {
        let price = ctx.accounts.listing.price;
        let fee_bps = ctx.accounts.marketplace.fee_bps as u128;

        let fee_wide = (price as u128)
            .checked_mul(fee_bps)
            .ok_or(MarketError::MathOverflow)?
            .checked_div(10_000)
            .ok_or(MarketError::MathOverflow)?;
        let fee = u64::try_from(fee_wide).map_err(|_| error!(MarketError::MathOverflow))?;
        let seller_amount = price.checked_sub(fee).ok_or(MarketError::MathOverflow)?;

        system_program::transfer(
            CpiContext::new(
                ctx.accounts.system_program.key(),
                SystemTransfer {
                    from: ctx.accounts.buyer.to_account_info(),
                    to: ctx.accounts.seller.to_account_info(),
                },
            ),
            seller_amount,
        )?;

        if fee > 0 {
            system_program::transfer(
                CpiContext::new(
                    ctx.accounts.system_program.key(),
                    SystemTransfer {
                        from: ctx.accounts.buyer.to_account_info(),
                        to: ctx.accounts.treasury.to_account_info(),
                    },
                ),
                fee,
            )?;
        }

        let mint_key = ctx.accounts.mint.key();
        let bump = ctx.accounts.listing.bump;
        let seeds: &[&[u8]] = &[b"listing", mint_key.as_ref(), &[bump]];
        let signer_seeds: &[&[&[u8]]] = &[seeds];

        token::transfer(
            CpiContext::new_with_signer(
                ctx.accounts.token_program.key(),
                Transfer {
                    from: ctx.accounts.escrow_token_account.to_account_info(),
                    to: ctx.accounts.buyer_token_account.to_account_info(),
                    authority: ctx.accounts.listing.to_account_info(),
                },
                signer_seeds,
            ),
            1,
        )?;

        // Close the empty escrow account and refund its rent to the seller.
        token::close_account(CpiContext::new_with_signer(
            ctx.accounts.token_program.key(),
            CloseAccount {
                account: ctx.accounts.escrow_token_account.to_account_info(),
                destination: ctx.accounts.seller.to_account_info(),
                authority: ctx.accounts.listing.to_account_info(),
            },
            signer_seeds,
        ))?;

        emit!(Sold {
            seller: ctx.accounts.seller.key(),
            buyer: ctx.accounts.buyer.key(),
            mint: mint_key,
            price,
            fee,
        });
        // The listing account itself is closed by `close = seller` on the struct.
        Ok(())
    }

    /// Seller takes the NFT back and the listing is removed.
    pub fn cancel_listing(ctx: Context<CancelListing>) -> Result<()> {
        let mint_key = ctx.accounts.mint.key();
        let bump = ctx.accounts.listing.bump;
        let seeds: &[&[u8]] = &[b"listing", mint_key.as_ref(), &[bump]];
        let signer_seeds: &[&[&[u8]]] = &[seeds];

        token::transfer(
            CpiContext::new_with_signer(
                ctx.accounts.token_program.key(),
                Transfer {
                    from: ctx.accounts.escrow_token_account.to_account_info(),
                    to: ctx.accounts.seller_token_account.to_account_info(),
                    authority: ctx.accounts.listing.to_account_info(),
                },
                signer_seeds,
            ),
            1,
        )?;

        token::close_account(CpiContext::new_with_signer(
            ctx.accounts.token_program.key(),
            CloseAccount {
                account: ctx.accounts.escrow_token_account.to_account_info(),
                destination: ctx.accounts.seller.to_account_info(),
                authority: ctx.accounts.listing.to_account_info(),
            },
            signer_seeds,
        ))?;

        emit!(Cancelled {
            seller: ctx.accounts.seller.key(),
            mint: mint_key,
        });
        Ok(())
    }
}

// ---------------------------------------------------------------- state

#[account]
pub struct Marketplace {
    pub admin: Pubkey,    // 32
    pub treasury: Pubkey, // 32
    pub fee_bps: u16,     // 2
    pub bump: u8,         // 1
}

impl Marketplace {
    pub const SPACE: usize = 8 + 32 + 32 + 2 + 1;
}

#[account]
pub struct Listing {
    pub seller: Pubkey, // 32
    pub mint: Pubkey,   // 32
    pub price: u64,     // 8 (lamports)
    pub bump: u8,       // 1
}

impl Listing {
    pub const SPACE: usize = 8 + 32 + 32 + 8 + 1;
}

// --------------------------------------------------------------- events

#[event]
pub struct Listed {
    pub seller: Pubkey,
    pub mint: Pubkey,
    pub price: u64,
}

#[event]
pub struct Sold {
    pub seller: Pubkey,
    pub buyer: Pubkey,
    pub mint: Pubkey,
    pub price: u64,
    pub fee: u64,
}

#[event]
pub struct Cancelled {
    pub seller: Pubkey,
    pub mint: Pubkey,
}

// ------------------------------------------------------------- accounts

#[derive(Accounts)]
pub struct InitializeMarketplace<'info> {
    #[account(mut)]
    pub admin: Signer<'info>,

    /// Wallet that receives marketplace fees.
    pub treasury: SystemAccount<'info>,

    #[account(
        init,
        payer = admin,
        space = Marketplace::SPACE,
        seeds = [b"marketplace"],
        bump
    )]
    pub marketplace: Account<'info, Marketplace>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct ListNft<'info> {
    #[account(mut)]
    pub seller: Signer<'info>,

    pub mint: Account<'info, Mint>,

    #[account(
        mut,
        token::mint = mint,
        token::authority = seller
    )]
    pub seller_token_account: Account<'info, TokenAccount>,

    #[account(
        init,
        payer = seller,
        space = Listing::SPACE,
        seeds = [b"listing", mint.key().as_ref()],
        bump
    )]
    pub listing: Account<'info, Listing>,

    #[account(
        init,
        payer = seller,
        associated_token::mint = mint,
        associated_token::authority = listing
    )]
    pub escrow_token_account: Account<'info, TokenAccount>,

    pub token_program: Program<'info, Token>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct BuyNft<'info> {
    #[account(mut)]
    pub buyer: Signer<'info>,

    /// Receives the sale proceeds and the rent refunds.
    #[account(mut)]
    pub seller: SystemAccount<'info>,

    #[account(
        mut,
        has_one = treasury
    )]
    pub marketplace: Account<'info, Marketplace>,

    #[account(mut)]
    pub treasury: SystemAccount<'info>,

    pub mint: Account<'info, Mint>,

    #[account(
        mut,
        seeds = [b"listing", mint.key().as_ref()],
        bump = listing.bump,
        has_one = seller,
        has_one = mint,
        close = seller
    )]
    pub listing: Account<'info, Listing>,

    #[account(
        mut,
        associated_token::mint = mint,
        associated_token::authority = listing
    )]
    pub escrow_token_account: Account<'info, TokenAccount>,

    /// The buyer's token account for this NFT must already exist.
    #[account(
        mut,
        token::mint = mint,
        token::authority = buyer
    )]
    pub buyer_token_account: Account<'info, TokenAccount>,

    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct CancelListing<'info> {
    #[account(mut)]
    pub seller: Signer<'info>,

    pub mint: Account<'info, Mint>,

    #[account(
        mut,
        seeds = [b"listing", mint.key().as_ref()],
        bump = listing.bump,
        has_one = seller,
        has_one = mint,
        close = seller
    )]
    pub listing: Account<'info, Listing>,

    #[account(
        mut,
        associated_token::mint = mint,
        associated_token::authority = listing
    )]
    pub escrow_token_account: Account<'info, TokenAccount>,

    #[account(
        mut,
        token::mint = mint,
        token::authority = seller
    )]
    pub seller_token_account: Account<'info, TokenAccount>,

    pub token_program: Program<'info, Token>,
}

// --------------------------------------------------------------- errors

#[error_code]
pub enum MarketError {
    #[msg("Fee exceeds the maximum allowed.")]
    InvalidFee,
    #[msg("Price must be greater than zero.")]
    InvalidPrice,
    #[msg("Only NFTs can be listed: zero decimals and a supply of one.")]
    NotAnNft,
    #[msg("The mint authority must be revoked before listing.")]
    MintAuthorityNotRevoked,
    #[msg("The seller does not hold this NFT.")]
    NftNotOwned,
    #[msg("Arithmetic overflow.")]
    MathOverflow,
}
