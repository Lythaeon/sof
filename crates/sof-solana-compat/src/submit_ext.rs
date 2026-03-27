use async_trait::async_trait;
use bincode::serialize;
use solana_signer::signers::Signers;
use solana_transaction::versioned::VersionedTransaction;
use thiserror::Error;

use crate::{BuilderError, TxBuilder};

/// Error returned by the Solana-coupled submission helpers.
#[derive(Debug, Error)]
pub enum SolanaCompatSubmitError {
    /// Transaction building or signing failed before submission.
    #[error("failed to build/sign transaction: {source}")]
    Build {
        /// Underlying builder/signing error.
        source: BuilderError,
    },
    /// Core byte-oriented submit path failed.
    #[error(transparent)]
    Submit {
        /// Underlying core submit error.
        source: sof_tx::SubmitError,
    },
    /// Could not encode the signed transaction into bytes.
    #[error("failed to encode signed transaction bytes: {message}")]
    Encode {
        /// Encoder error details.
        message: String,
    },
}

/// Solana-coupled convenience methods layered on top of `sof-tx` core byte submission.
#[async_trait]
pub trait TxSubmitClientSolanaExt {
    /// Builds, signs, and submits a transaction in one API call.
    async fn submit_unsigned<T>(
        &mut self,
        builder: TxBuilder,
        signers: &T,
        mode: sof_tx::SubmitMode,
    ) -> Result<sof_tx::SubmitResult, SolanaCompatSubmitError>
    where
        T: Signers + Sync + ?Sized;

    /// Builds, signs, and submits a transaction with explicit toxic-flow context.
    async fn submit_unsigned_with_context<T>(
        &mut self,
        builder: TxBuilder,
        signers: &T,
        mode: sof_tx::SubmitMode,
        context: sof_tx::TxSubmitContext,
    ) -> Result<sof_tx::SubmitResult, SolanaCompatSubmitError>
    where
        T: Signers + Sync + ?Sized;

    /// Submits one signed `VersionedTransaction`.
    async fn submit_transaction(
        &mut self,
        tx: VersionedTransaction,
        mode: sof_tx::SubmitMode,
    ) -> Result<sof_tx::SubmitResult, SolanaCompatSubmitError>;

    /// Submits one signed `VersionedTransaction` with explicit toxic-flow context.
    async fn submit_transaction_with_context(
        &mut self,
        tx: VersionedTransaction,
        mode: sof_tx::SubmitMode,
        context: sof_tx::TxSubmitContext,
    ) -> Result<sof_tx::SubmitResult, SolanaCompatSubmitError>;
}

#[async_trait]
impl TxSubmitClientSolanaExt for sof_tx::TxSubmitClient {
    async fn submit_unsigned<T>(
        &mut self,
        builder: TxBuilder,
        signers: &T,
        mode: sof_tx::SubmitMode,
    ) -> Result<sof_tx::SubmitResult, SolanaCompatSubmitError>
    where
        T: Signers + Sync + ?Sized,
    {
        self.submit_unsigned_with_context(
            builder,
            signers,
            mode,
            sof_tx::TxSubmitContext::default(),
        )
        .await
    }

    async fn submit_unsigned_with_context<T>(
        &mut self,
        builder: TxBuilder,
        signers: &T,
        mode: sof_tx::SubmitMode,
        context: sof_tx::TxSubmitContext,
    ) -> Result<sof_tx::SubmitResult, SolanaCompatSubmitError>
    where
        T: Signers + Sync + ?Sized,
    {
        let blockhash = self
            .refresh_latest_blockhash_bytes()
            .await
            .map_err(|source| SolanaCompatSubmitError::Submit {
                source: sof_tx::SubmitError::Rpc { source },
            })?
            .ok_or_else(|| SolanaCompatSubmitError::Submit {
                source: sof_tx::SubmitError::MissingRecentBlockhash,
            })?;
        let tx = builder
            .build_and_sign(blockhash, signers)
            .map_err(|source| SolanaCompatSubmitError::Build { source })?;
        self.submit_transaction_with_context(tx, mode, context)
            .await
    }

    async fn submit_transaction(
        &mut self,
        tx: VersionedTransaction,
        mode: sof_tx::SubmitMode,
    ) -> Result<sof_tx::SubmitResult, SolanaCompatSubmitError> {
        self.submit_transaction_with_context(tx, mode, sof_tx::TxSubmitContext::default())
            .await
    }

    async fn submit_transaction_with_context(
        &mut self,
        tx: VersionedTransaction,
        mode: sof_tx::SubmitMode,
        context: sof_tx::TxSubmitContext,
    ) -> Result<sof_tx::SubmitResult, SolanaCompatSubmitError> {
        let tx_bytes = serialize(&tx).map_err(|error| SolanaCompatSubmitError::Encode {
            message: error.to_string(),
        })?;
        self.submit_signed_with_context(
            sof_tx::SignedTx::VersionedTransactionBytes(tx_bytes),
            mode,
            context,
        )
        .await
        .map_err(|source| SolanaCompatSubmitError::Submit { source })
    }
}
