use solana_signature::Signature;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
/// Transaction classification used by runtime and plugins.
pub enum TxKind {
    /// Contains only vote program instructions.
    VoteOnly,
    /// Contains a mix of vote and non-vote instructions.
    Mixed,
    /// Contains no vote program instructions.
    NonVote,
}

#[derive(Debug, Clone, Eq, PartialEq)]
/// Event emitted by runtime when a transaction is observed.
pub struct TxObservedEvent {
    /// Slot where transaction was observed.
    pub slot: u64,
    /// Transaction signature.
    pub signature: Signature,
    /// Transaction kind classification.
    pub kind: TxKind,
}
