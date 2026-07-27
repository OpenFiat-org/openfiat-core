use anchor_lang::prelude::*;

#[error_code]
pub enum ErrorCode {
    #[msg("Only the sale admin may perform this action")]
    Unauthorized,
}
