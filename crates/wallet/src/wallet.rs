//! Key management: a `Wallet` owns one keypair and derives everything
//! else (`PeerId`, signatures) from it, the same identity primitive every
//! other crate in this workspace already signs its events with.

use openfiat_crypto::Keypair;
use openfiat_network::identity::peer_id_from_public_key;
use openfiat_types::{PeerId, PublicKey, Signature};

pub struct Wallet {
    keypair: Keypair,
    peer_id: PeerId,
}

impl Wallet {
    pub fn generate() -> Self {
        Self::from_keypair(Keypair::generate())
    }

    pub fn from_seed(seed: [u8; 32]) -> Self {
        Self::from_keypair(Keypair::from_seed(seed))
    }

    fn from_keypair(keypair: Keypair) -> Self {
        let peer_id = peer_id_from_public_key(&keypair.public_key())
            .expect("a freshly generated keypair's public key always derives a peer id");
        Self { keypair, peer_id }
    }

    pub fn peer_id(&self) -> PeerId {
        self.peer_id.clone()
    }

    pub fn public_key(&self) -> PublicKey {
        self.keypair.public_key()
    }

    /// This wallet's Solana address, base58 — the string an operator
    /// pastes into an explorer, a faucet, or a stake instruction.
    ///
    /// It is the same 32 bytes as [`Wallet::public_key`], in the encoding
    /// every other tool in this ecosystem prints. That matters more than
    /// it sounds: a node that logged its identity as a byte array left an
    /// operator with a number they could not look up anywhere, could not
    /// search for, and could not compare against the address their wallet
    /// showed them, even though it was the same key.
    pub fn address(&self) -> String {
        bs58::encode(self.public_key().as_bytes()).into_string()
    }

    /// The seed this wallet was derived from — the caller's
    /// responsibility to store securely; this crate does no disk
    /// persistence of its own.
    pub fn seed(&self) -> [u8; 32] {
        self.keypair.seed()
    }

    /// Raw message signing — the primitive every domain crate's own
    /// signed-event types build on, and what a Solana instruction builder
    /// would call directly over a transaction message's bytes.
    pub fn sign(&self, message: &[u8]) -> Signature {
        self.keypair.sign(message)
    }
}
