//! High-level transaction builder APIs.

use solana_compute_budget_interface::ComputeBudgetInstruction;
use solana_message::{Hash, Instruction, Message, VersionedMessage};
use solana_pubkey::Pubkey;
use solana_signer::{SignerError, signers::Signers};
use solana_system_interface::instruction as system_instruction;
use solana_transaction::versioned::VersionedTransaction;
use thiserror::Error;

/// Default lamports tipped by [`TxBuilder::tip_developer`].
pub const DEFAULT_DEVELOPER_TIP_LAMPORTS: u64 = 5_000;

/// Default developer tip recipient used by [`TxBuilder::tip_developer`].
pub const DEFAULT_DEVELOPER_TIP_RECIPIENT: Pubkey =
    Pubkey::from_str_const("G3WHMVjx7Cb3MFhBAHe52zw8yhbHodWnas5gYLceaqze");

/// Builder-layer errors.
#[derive(Debug, Error)]
pub enum BuilderError {
    /// Signing failed with signer-level error.
    #[error("failed to sign transaction: {source}")]
    SignTransaction {
        /// Underlying signer error.
        source: SignerError,
    },
}

/// Unsigned transaction wrapper.
#[derive(Debug, Clone)]
pub struct UnsignedTx {
    /// Versioned message ready to sign.
    message: VersionedMessage,
}

impl UnsignedTx {
    /// Returns the message payload.
    #[must_use]
    pub const fn message(&self) -> &VersionedMessage {
        &self.message
    }

    /// Signs the message with provided signers.
    ///
    /// # Errors
    ///
    /// Returns [`BuilderError::SignTransaction`] when signer validation or signing fails.
    pub fn sign<T>(self, signers: &T) -> Result<VersionedTransaction, BuilderError>
    where
        T: Signers + ?Sized,
    {
        VersionedTransaction::try_new(self.message, signers)
            .map_err(|source| BuilderError::SignTransaction { source })
    }
}

/// High-level builder for legacy-versioned transaction messages.
#[derive(Debug, Clone)]
pub struct TxBuilder {
    /// Fee payer and signer.
    payer: Pubkey,
    /// User-provided instructions.
    instructions: Vec<Instruction>,
    /// Optional compute unit limit.
    compute_unit_limit: Option<u32>,
    /// Optional priority fee (micro-lamports per compute unit).
    priority_fee_micro_lamports: Option<u64>,
    /// Optional developer tip lamports.
    developer_tip_lamports: Option<u64>,
    /// Tip recipient used when tip is enabled.
    developer_tip_recipient: Pubkey,
}

impl TxBuilder {
    /// Creates a transaction builder for a fee payer.
    #[must_use]
    pub const fn new(payer: Pubkey) -> Self {
        Self {
            payer,
            instructions: Vec::new(),
            compute_unit_limit: None,
            priority_fee_micro_lamports: None,
            developer_tip_lamports: None,
            developer_tip_recipient: DEFAULT_DEVELOPER_TIP_RECIPIENT,
        }
    }

    /// Appends one instruction.
    #[must_use]
    pub fn add_instruction(mut self, instruction: Instruction) -> Self {
        self.instructions.push(instruction);
        self
    }

    /// Appends many instructions.
    #[must_use]
    pub fn add_instructions<I>(mut self, instructions: I) -> Self
    where
        I: IntoIterator<Item = Instruction>,
    {
        self.instructions.extend(instructions);
        self
    }

    /// Sets compute unit limit.
    #[must_use]
    pub const fn with_compute_unit_limit(mut self, units: u32) -> Self {
        self.compute_unit_limit = Some(units);
        self
    }

    /// Sets priority fee in micro-lamports.
    #[must_use]
    pub const fn with_priority_fee_micro_lamports(mut self, micro_lamports: u64) -> Self {
        self.priority_fee_micro_lamports = Some(micro_lamports);
        self
    }

    /// Enables default developer tip.
    #[must_use]
    pub const fn tip_developer(mut self) -> Self {
        self.developer_tip_lamports = Some(DEFAULT_DEVELOPER_TIP_LAMPORTS);
        self
    }

    /// Enables developer tip with explicit lamports.
    #[must_use]
    pub const fn tip_developer_lamports(mut self, lamports: u64) -> Self {
        self.developer_tip_lamports = Some(lamports);
        self
    }

    /// Sets a custom tip recipient and lamports.
    #[must_use]
    pub const fn tip_to(mut self, recipient: Pubkey, lamports: u64) -> Self {
        self.developer_tip_recipient = recipient;
        self.developer_tip_lamports = Some(lamports);
        self
    }

    /// Builds an unsigned transaction wrapper.
    #[must_use]
    pub fn build_unsigned(self, recent_blockhash: [u8; 32]) -> UnsignedTx {
        UnsignedTx {
            message: self.build_message(recent_blockhash),
        }
    }

    /// Builds and signs a transaction in one step.
    ///
    /// # Errors
    ///
    /// Returns [`BuilderError::SignTransaction`] when signer validation or signing fails.
    pub fn build_and_sign<T>(
        self,
        recent_blockhash: [u8; 32],
        signers: &T,
    ) -> Result<VersionedTransaction, BuilderError>
    where
        T: Signers + ?Sized,
    {
        self.build_unsigned(recent_blockhash).sign(signers)
    }

    /// Builds a legacy message wrapped as a versioned message.
    #[must_use]
    pub fn build_message(self, recent_blockhash: [u8; 32]) -> VersionedMessage {
        let mut instructions = Vec::new();
        if let Some(units) = self.compute_unit_limit {
            instructions.push(ComputeBudgetInstruction::set_compute_unit_limit(units));
        }
        if let Some(micro_lamports) = self.priority_fee_micro_lamports {
            instructions.push(ComputeBudgetInstruction::set_compute_unit_price(
                micro_lamports,
            ));
        }
        instructions.extend(self.instructions);
        if let Some(lamports) = self.developer_tip_lamports {
            instructions.push(system_instruction::transfer(
                &self.payer,
                &self.developer_tip_recipient,
                lamports,
            ));
        }
        let blockhash = Hash::new_from_array(recent_blockhash);
        let message = Message::new_with_blockhash(&instructions, Some(&self.payer), &blockhash);
        VersionedMessage::Legacy(message)
    }
}

#[cfg(test)]
mod tests {
    use solana_keypair::Keypair;
    use solana_signer::Signer;

    use super::*;

    #[test]
    fn tip_developer_adds_system_transfer_instruction() {
        let payer = Keypair::new();
        let message = TxBuilder::new(payer.pubkey())
            .tip_developer()
            .build_message([1_u8; 32]);

        let keys = message.static_account_keys();
        let instructions = message.instructions();
        assert_eq!(instructions.len(), 1);

        let first = instructions.first();
        assert!(first.is_some());
        if let Some(instruction) = first {
            let program_idx = usize::from(instruction.program_id_index);
            let program = keys.get(program_idx);
            assert!(program.is_some());
            if let Some(program) = program {
                assert_eq!(*program, solana_system_interface::program::ID);
            }
        }
    }

    #[test]
    fn compute_budget_instructions_are_prefixed() {
        let payer = Keypair::new();
        let recipient = Pubkey::new_unique();
        let message = TxBuilder::new(payer.pubkey())
            .with_compute_unit_limit(500_000)
            .with_priority_fee_micro_lamports(10_000)
            .add_instruction(system_instruction::transfer(&payer.pubkey(), &recipient, 1))
            .build_message([2_u8; 32]);

        let instructions = message.instructions();
        assert_eq!(instructions.len(), 3);
        let first = instructions.first();
        assert!(first.is_some());
        if let Some(first) = first {
            assert_eq!(first.data.first().copied(), Some(2_u8));
        }
        let second = instructions.get(1);
        assert!(second.is_some());
        if let Some(second) = second {
            assert_eq!(second.data.first().copied(), Some(3_u8));
        }
    }

    #[test]
    fn build_and_sign_generates_signature() {
        let payer = Keypair::new();
        let recipient = Pubkey::new_unique();
        let tx_result = TxBuilder::new(payer.pubkey())
            .add_instruction(system_instruction::transfer(&payer.pubkey(), &recipient, 1))
            .build_and_sign([3_u8; 32], &[&payer]);

        assert!(tx_result.is_ok());
        if let Ok(tx) = tx_result {
            assert_eq!(tx.signatures.len(), 1);
            let first = tx.signatures.first();
            assert!(first.is_some());
            if let Some(first) = first {
                assert_ne!(*first, solana_signature::Signature::default());
            }
        }
    }
}
