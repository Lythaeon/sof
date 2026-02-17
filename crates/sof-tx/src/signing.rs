//! Signing boundary adapters used by the builder/submission APIs.

use solana_signer::Signer;

/// Borrowed signer reference wrapper.
#[derive(Clone, Copy)]
pub struct SignerRef<'signer> {
    /// Borrowed signer object.
    signer: &'signer dyn Signer,
}

impl<'signer> SignerRef<'signer> {
    /// Creates a signer reference wrapper.
    #[must_use]
    pub fn new(signer: &'signer dyn Signer) -> Self {
        Self { signer }
    }

    /// Returns the wrapped signer.
    #[must_use]
    pub fn as_signer(self) -> &'signer dyn Signer {
        self.signer
    }
}
