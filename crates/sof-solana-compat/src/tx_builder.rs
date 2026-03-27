use solana_compute_budget_interface::ComputeBudgetInstruction;
use solana_message::{Hash, Instruction, Message, VersionedMessage, v0};
use solana_packet::PACKET_DATA_SIZE;
use solana_pubkey::Pubkey;
use solana_signer::{Signer, SignerError, signers::Signers};
use solana_system_interface::instruction as system_instruction;
use solana_transaction::sanitized::MAX_TX_ACCOUNT_LOCKS as SOLANA_MAX_TX_ACCOUNT_LOCKS;
use solana_transaction::versioned::VersionedTransaction;
use thiserror::Error;

/// Default lamports tipped by [`TxBuilder::tip_developer`].
pub const DEFAULT_DEVELOPER_TIP_LAMPORTS: u64 = 5_000;

/// Default developer tip recipient used by [`TxBuilder::tip_developer`].
pub const DEFAULT_DEVELOPER_TIP_RECIPIENT: Pubkey =
    Pubkey::from_str_const("G3WHMVjx7Cb3MFhBAHe52zw8yhbHodWnas5gYLceaqze");

/// Current maximum serialized transaction payload size accepted by Solana networking.
pub const MAX_TRANSACTION_WIRE_BYTES: usize = PACKET_DATA_SIZE;

/// Current maximum number of account locks allowed per transaction.
pub const MAX_TRANSACTION_ACCOUNT_LOCKS: usize = SOLANA_MAX_TX_ACCOUNT_LOCKS;

/// Transaction message version emitted by [`TxBuilder`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TxMessageVersion {
    /// Legacy message encoding.
    Legacy,
    /// Version 0 message encoding.
    #[default]
    V0,
}

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

/// High-level builder for Solana versioned transaction messages.
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
    /// Message version emitted by the builder.
    message_version: TxMessageVersion,
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
            message_version: TxMessageVersion::V0,
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

    /// Removes any explicit compute unit limit instruction.
    #[must_use]
    pub const fn without_compute_unit_limit(mut self) -> Self {
        self.compute_unit_limit = None;
        self
    }

    /// Sets priority fee in micro-lamports.
    #[must_use]
    pub const fn with_priority_fee_micro_lamports(mut self, micro_lamports: u64) -> Self {
        self.priority_fee_micro_lamports = Some(micro_lamports);
        self
    }

    /// Removes any explicit priority-fee instruction.
    #[must_use]
    pub const fn without_priority_fee_micro_lamports(mut self) -> Self {
        self.priority_fee_micro_lamports = None;
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

    /// Sets the message version emitted by the builder.
    #[must_use]
    pub const fn with_message_version(mut self, version: TxMessageVersion) -> Self {
        self.message_version = version;
        self
    }

    /// Forces legacy message output.
    #[must_use]
    pub const fn with_legacy_message(self) -> Self {
        self.with_message_version(TxMessageVersion::Legacy)
    }

    /// Forces version 0 message output.
    #[must_use]
    pub const fn with_v0_message(self) -> Self {
        self.with_message_version(TxMessageVersion::V0)
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

    /// Builds a message wrapped as a versioned message.
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
        let legacy_message =
            Message::new_with_blockhash(&instructions, Some(&self.payer), &blockhash);
        match self.message_version {
            TxMessageVersion::Legacy => VersionedMessage::Legacy(legacy_message),
            TxMessageVersion::V0 => VersionedMessage::V0(v0::Message {
                header: legacy_message.header,
                account_keys: legacy_message.account_keys,
                recent_blockhash: legacy_message.recent_blockhash,
                instructions: legacy_message.instructions,
                address_table_lookups: Vec::new(),
            }),
        }
    }
}

/// Borrowed signer reference for Solana-coupled call sites.
#[derive(Clone, Copy)]
pub struct SignerRef<'signer> {
    /// Borrowed signer trait object.
    signer: &'signer dyn Signer,
}

impl<'signer> SignerRef<'signer> {
    /// Creates one borrowed signer reference.
    #[must_use]
    pub fn new(signer: &'signer dyn Signer) -> Self {
        Self { signer }
    }

    /// Returns the borrowed signer trait object.
    #[must_use]
    pub fn as_signer(self) -> &'signer dyn Signer {
        self.signer
    }
}
