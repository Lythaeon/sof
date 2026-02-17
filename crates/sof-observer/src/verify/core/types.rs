#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum VerifyStatus {
    Verified,
    UnknownLeader,
    InvalidMerkle,
    InvalidSignature,
    Malformed,
}

impl VerifyStatus {
    #[must_use]
    pub const fn is_accepted(self, strict_unknown: bool) -> bool {
        match self {
            Self::Verified => true,
            Self::UnknownLeader => !strict_unknown,
            Self::InvalidMerkle | Self::InvalidSignature | Self::Malformed => false,
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(super) enum ShredKind {
    Data,
    Code,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(super) struct Variant {
    pub kind: ShredKind,
    pub proof_size: u8,
    pub resigned: bool,
}
