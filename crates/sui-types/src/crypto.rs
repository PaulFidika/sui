// Copyright (c) 2022, Mysten Labs, Inc.
// SPDX-License-Identifier: Apache-2.0
use std::fmt::Display;
use std::hash::{Hash, Hasher};
use std::str::FromStr;

use anyhow::Error;
use base64ct::Encoding;
use digest::Digest;
use narwhal_crypto::bls12381::{BLS12381Signature, BLS12381PublicKey, BLS12381KeyPair};
use narwhal_crypto::ed25519::{
    Ed25519AggregateSignature, Ed25519KeyPair, Ed25519PrivateKey, Ed25519PublicKey,
    Ed25519Signature,
};
pub use narwhal_crypto::traits::KeyPair as KeypairTraits;
pub use narwhal_crypto::traits::{
    AggregateAuthenticator, Authenticator, SigningKey, ToFromBytes, VerifyingKey,
};
use narwhal_crypto::Verifier;
use rand::rngs::OsRng;
use roaring::RoaringBitmap;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_with::{serde_as, Bytes};
use sha3::Sha3_256;

use crate::base_types::{AuthorityName, SuiAddress};
use crate::committee::{Committee, EpochId};
use crate::error::{SuiError, SuiResult};
use crate::sui_serde::{Base64, Readable, SuiBitmap};

// Comment the one you want to use

// Authority Objects
pub type AuthorityKeyPair = Ed25519KeyPair;
pub type AuthorityPrivateKey = Ed25519PrivateKey;
pub type AuthorityPublicKey = Ed25519PublicKey;
pub type AuthoritySignature = Ed25519Signature;
pub type AggregateAuthoritySignature = Ed25519AggregateSignature;

// Account Objects
pub type AccountKeyPair = Ed25519KeyPair;
pub type AccountPublicKey = Ed25519PublicKey;
pub type AccountPrivateKey = Ed25519PrivateKey;
pub type AccountSignature = Ed25519Signature;
pub type AggregateAccountSignature = Ed25519AggregateSignature;
//
// Define Bytes representation of the Authority's PublicKey
//

#[serde_as]
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct AuthorityPublicKeyBytes(#[serde_as(as = "Bytes")] [u8; AuthorityPublicKey::LENGTH]);

impl TryFrom<AuthorityPublicKeyBytes> for AuthorityPublicKey {
    type Error = signature::Error;

    fn try_from(bytes: AuthorityPublicKeyBytes) -> Result<AuthorityPublicKey, Self::Error> {
        AuthorityPublicKey::from_bytes(bytes.as_ref()).map_err(|_| Self::Error::new())
    }
}

impl From<&AuthorityPublicKey> for AuthorityPublicKeyBytes {
    fn from(pk: &AuthorityPublicKey) -> AuthorityPublicKeyBytes {
        AuthorityPublicKeyBytes::from_bytes(pk.as_ref()).unwrap()
    }
}

impl AsRef<[u8]> for AuthorityPublicKeyBytes {
    fn as_ref(&self) -> &[u8] {
        &self.0[..]
    }
}

impl Display for AuthorityPublicKeyBytes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::result::Result<(), std::fmt::Error> {
        let s = hex::encode(&self.0);
        write!(f, "k#{}", s)?;
        Ok(())
    }
}

impl ToFromBytes for AuthorityPublicKeyBytes {
    fn from_bytes(bytes: &[u8]) -> Result<Self, signature::Error> {
        let bytes: [u8; AuthorityPublicKey::LENGTH] =
            bytes.try_into().map_err(signature::Error::from_source)?;
        Ok(AuthorityPublicKeyBytes(bytes))
    }
}

impl AuthorityPublicKeyBytes {
    /// This ensures it's impossible to construct an instance with other than registered lengths
    pub fn new(bytes: [u8; AuthorityPublicKey::LENGTH]) -> AuthorityPublicKeyBytes
where {
        AuthorityPublicKeyBytes(bytes)
    }

    // this is probably derivable, but we'd rather have it explicitly laid out for instructional purposes,
    // see [#34](https://github.com/MystenLabs/narwhal/issues/34)
    #[allow(dead_code)]
    fn default() -> Self {
        Self([0u8; AuthorityPublicKey::LENGTH])
    }
}

impl FromStr for AuthorityPublicKeyBytes {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.strip_prefix("0x").unwrap_or(s);
        let value = hex::decode(s)?;
        Self::from_bytes(&value[..]).map_err(|_| anyhow::anyhow!("byte deserialization failed"))
    }
}

//
// Add helper calls for Authority Signature
//

pub trait SuiAuthoritySignature {
    fn new<T>(value: &T, secret: &dyn signature::Signer<Self>) -> Self
    where
        T: Signable<Vec<u8>>;

    fn verify<T>(&self, value: &T, author: AuthorityPublicKeyBytes) -> Result<(), SuiError>
    where
        T: Signable<Vec<u8>>;
}

impl SuiAuthoritySignature for AuthoritySignature {
    fn new<T>(value: &T, secret: &dyn signature::Signer<Self>) -> Self
    where
        T: Signable<Vec<u8>>,
    {
        let mut message = Vec::new();
        value.write(&mut message);
        secret.sign(&message)
    }

    fn verify<T>(&self, value: &T, author: AuthorityPublicKeyBytes) -> Result<(), SuiError>
    where
        T: Signable<Vec<u8>>,
    {
        // is this a cryptographically valid public Key?
        let public_key =
            AuthorityPublicKey::from_bytes(author.as_ref()).map_err(|_| SuiError::InvalidAddress)?;
        // serialize the message (see BCS serialization for determinism)
        let mut message = Vec::new();
        value.write(&mut message);

        // perform cryptographic signature check
        public_key
            .verify(&message, self)
            .map_err(|error| SuiError::InvalidSignature {
                error: error.to_string(),
            })
    }
}

impl signature::Verifier<Signature> for AuthorityPublicKeyBytes {
    fn verify(&self, message: &[u8], signature: &Signature) -> Result<(), signature::Error> {
        // deserialize the signature
        let signature =
            <AccountSignature as signature::Signature>::from_bytes(signature.signature_bytes())
                .map_err(|_| signature::Error::new())?;

        let public_key =
            AuthorityPublicKey::from_bytes(self.as_ref()).map_err(|_| signature::Error::new())?;

        // perform cryptographic signature check
        public_key
            .verify(message, &signature)
            .map_err(|_| signature::Error::new())
    }
}

pub fn random_key_pairs<KP: KeypairTraits>(num: usize) -> Vec<KP> {
    let mut items = num;
    let mut rng = OsRng;

    std::iter::from_fn(|| {
        if items == 0 {
            None
        } else {
            items -= 1;
            Some(get_key_pair_from_rng(&mut rng).1)
        }
    })
    .collect::<Vec<_>>()
}

// TODO: get_key_pair() and get_key_pair_from_bytes() should return KeyPair only.
// TODO: rename to random_key_pair
pub fn get_key_pair<KP: KeypairTraits>() -> (SuiAddress, KP) {
    get_key_pair_from_rng(&mut OsRng)
}

/// Generate a keypair from the specified RNG (useful for testing with seedable rngs).
pub fn get_key_pair_from_rng<KP: KeypairTraits, R>(csprng: &mut R) -> (SuiAddress, KP)
where
    R: rand::CryptoRng + rand::RngCore,
{
    let kp = KP::generate(csprng);
    (kp.public().into(), kp)
}

// TODO: C-GETTER
pub fn get_key_pair_from_bytes<KP: KeypairTraits>(bytes: &[u8]) -> SuiResult<(SuiAddress, KP)> {
    let priv_length = <KP as KeypairTraits>::PrivKey::LENGTH;
    let sk =
        <KP as KeypairTraits>::PrivKey::from_bytes(&bytes[..priv_length]).map_err(|_| SuiError::InvalidPrivateKey)?;
    let kp: KP = sk.into();
    if kp.public().as_ref() != &bytes[priv_length..] {
        return Err(SuiError::InvalidAddress);
    }
    Ok((kp.public().into(), kp))
}

// 
// Account Signatures
// 

// 1. Eddsa
// 2. Ecdsa
// Enums for Signatures
const FLAG_LENGTH: usize = 1;

#[derive(Clone, Serialize, Deserialize, JsonSchema)]
pub enum Signature {
    Ed25519(Ed25519SuiSignature),
    BLS12381(BLS12381SuiSignature),
    Empty
}


// Can refactor this with a library
impl Signature {
    pub fn verify<T>(&self, value: &T, author: SuiAddress) -> SuiResult<()> 
        where T: Signable<Vec<u8>>,
    {
        match self {
            Self::Ed25519(sig) => sig.verify(value, author),
            Self::BLS12381(sig) => sig.verify(value, author),
            Self::Empty => Err(SuiError::InvalidSignature {
                error: "Empty signature".to_string(),
            })
        }
    }

    pub fn public_key_bytes(&self) -> &[u8] {
        match self {
            Self::Ed25519(sig) => sig.public_key_bytes(),
            Self::BLS12381(sig) => sig.public_key_bytes(),
            Self::Empty => &[]
        }
    }

    pub fn FLAG_bytes(&self) -> &[u8] {
        match self {
            Self::Ed25519(sig) => sig.FLAG_bytes(),
            Self::BLS12381(sig) => sig.FLAG_bytes(),
            Self::Empty => &[]
        }
    }

    pub fn signature_bytes(&self) -> &[u8] {
        match self {
            Self::Ed25519(sig) => sig.signature_bytes(),
            Self::BLS12381(sig) => sig.signature_bytes(),
            Self::Empty => &[]
        }
    }

    pub fn new<T>(value: &T, secret: &dyn signature::Signer<Signature>) -> Signature 
    where
        T: Signable<Vec<u8>>,
    {
        let mut message = Vec::new();
        value.write(&mut message);
        secret.sign(&message)
    }
}

impl AsRef<[u8]> for Signature {
    fn as_ref(&self) -> &[u8] {
        match self {
            Self::Ed25519(sig) => sig.bytes(),
            Self::BLS12381(sig) => sig.bytes(),
            Self::Empty => &[]
        }
    }
}

impl signature::Signature for Signature {
    fn from_bytes(bytes: &[u8]) -> Result<Self, signature::Error> {
        match bytes.get(0..2).ok_or(signature::Error::new())? {
            x if x == &Ed25519SuiSignature::FLAG[..] => Ok(Signature::Ed25519(Ed25519SuiSignature::from_bytes(bytes).map_err(|_| signature::Error::new())?)),
            _ => Err(signature::Error::new()),
        }
    }
}

impl std::fmt::Debug for Signature {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::result::Result<(), std::fmt::Error> {
        let s = base64ct::Base64::encode_string(self.signature_bytes());
        let p = base64ct::Base64::encode_string(self.public_key_bytes());
        write!(f, "{s}@{p}")?;
        Ok(())
    }
}

// impl std::fmt::Debug for Signature {
//     fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::result::Result<(), std::fmt::Error> {
//         let s = base64ct::Base64::encode_string(self.signature_bytes());
//         let p = base64ct::Base64::encode_string(self.public_key_bytes());
//         write!(f, "{s}@{p}")?;
//         Ok(())
//     }
// }

// 
// BLS Port
// 


#[serde_as]
#[derive(Clone, Serialize, Deserialize, JsonSchema)]
pub struct BLS12381SuiSignature (
    #[schemars(with = "Base64")]
    #[serde_as(as = "Bytes")]
    [u8; Self::LENGTH]
);

impl SuiSignature for BLS12381SuiSignature {
    type Sig = BLS12381Signature; 
    type PubKey = BLS12381PublicKey;
    type KeyPair = BLS12381KeyPair;
    const FLAG: [u8; FLAG_LENGTH] = [0x25];

    fn bytes(&self) -> &[u8] {
        self.0.as_ref()
    }

    fn from_bytes(bytes: &[u8]) -> SuiResult<Self> {
        if bytes.len() != Self::LENGTH {
            return Err(SuiError::InvalidSignature {
                error: format!("Invalid signature length: {}", bytes.len()),
            });
        }
        let mut result_bytes = [0u8; Self::LENGTH];
        result_bytes.copy_from_slice(bytes);
        return Ok(Self(result_bytes));
    }
}

impl signature::Signer<Signature> for BLS12381KeyPair {
    fn try_sign(&self, msg: &[u8]) -> Result<Signature, signature::Error> {
        let signature_bytes: <<BLS12381KeyPair as KeypairTraits>::PrivKey as SigningKey>::Sig =
            self.try_sign(msg)?;

        let pk_bytes = self.public().as_ref();
        let mut result_bytes = [0u8; BLS12381SuiSignature::LENGTH];

        result_bytes[..FLAG_LENGTH].copy_from_slice(&BLS12381SuiSignature::FLAG);
        result_bytes[FLAG_LENGTH..<Self as KeypairTraits>::Sig::LENGTH + FLAG_LENGTH].copy_from_slice(&signature_bytes.as_ref());
        result_bytes[<Self as KeypairTraits>::Sig::LENGTH + FLAG_LENGTH..].copy_from_slice(pk_bytes);
        Ok(Signature::BLS12381(BLS12381SuiSignature(result_bytes)))
    }
}

// 
// Ed25519 Sui Signature port
// 
#[serde_as]
#[derive(Clone, Serialize, Deserialize, JsonSchema)]
pub struct Ed25519SuiSignature (
    #[schemars(with = "Base64")]
    #[serde_as(as = "Bytes")]
    [u8; Self::LENGTH]
);

impl SuiSignature for Ed25519SuiSignature {
    type Sig = Ed25519Signature; 
    type PubKey = Ed25519PublicKey;
    type KeyPair = Ed25519KeyPair;
    const LENGTH: usize = Ed25519PublicKey::LENGTH + Ed25519Signature::LENGTH + FLAG_LENGTH;
    const FLAG: [u8; FLAG_LENGTH] = [0xed];

    fn bytes(&self) -> &[u8] {
        self.0.as_ref()
    }

    fn from_bytes(bytes: &[u8]) -> SuiResult<Self> {
        if bytes.len() != Self::LENGTH {
            return Err(SuiError::InvalidSignature {
                error: format!("Invalid signature length: {}", bytes.len()),
            });
        }
        let mut result_bytes = [0u8; Self::LENGTH];
        result_bytes.copy_from_slice(bytes);
        return Ok(Ed25519SuiSignature(result_bytes));
    }
}

impl signature::Signer<Signature> for Ed25519KeyPair {
    fn try_sign(&self, msg: &[u8]) -> Result<Signature, signature::Error> {
        let signature_bytes: <<Ed25519KeyPair as KeypairTraits>::PrivKey as SigningKey>::Sig =
            self.try_sign(msg)?;

        let pk_bytes = self.public().as_ref();
        let mut result_bytes = [0u8; Ed25519SuiSignature::LENGTH];

        result_bytes[..FLAG_LENGTH].copy_from_slice(&Ed25519SuiSignature::FLAG);
        result_bytes[FLAG_LENGTH..<Self as KeypairTraits>::Sig::LENGTH + FLAG_LENGTH].copy_from_slice(&signature_bytes.as_ref());
        result_bytes[<Self as KeypairTraits>::Sig::LENGTH + FLAG_LENGTH..].copy_from_slice(pk_bytes);
        Ok(Signature::Ed25519(Ed25519SuiSignature(result_bytes)))
    }
}

// 
// SuiSignature
// 
pub trait SuiSignature: Sized {
    type Sig: Authenticator;
    type PubKey: VerifyingKey<Sig = Self::Sig>;
    type KeyPair: KeypairTraits<PubKey = Self::PubKey>;

    const FLAG: [u8; FLAG_LENGTH];
    const LENGTH: usize = Self::Sig::LENGTH + Self::PubKey::LENGTH + FLAG_LENGTH;

    fn from_bytes(bytes: &[u8]) -> SuiResult<Self>;
    fn bytes(&self) -> &[u8];

    fn FLAG_bytes(&self) -> &[u8] {
        &self.bytes()[..FLAG_LENGTH]
    }

    fn signature_bytes(&self) -> &[u8] {
        &self.bytes()[FLAG_LENGTH..Self::Sig::LENGTH + FLAG_LENGTH]
    }

    fn public_key_bytes(&self) -> &[u8] {
        &self.bytes()[FLAG_LENGTH + Self::Sig::LENGTH..]
    }

    fn verify<T>(&self, value: &T, author: SuiAddress) -> SuiResult<()>
    where
        T: Signable<Vec<u8>>,
    {
        let (signature, public_key) = self.get_verification_inputs::<T>(author)?;

        let mut message = Vec::new();
        value.write(&mut message);

        // perform cryptographic signature check
        public_key
            .verify(&message, &signature)
            .map_err(|error| SuiError::InvalidSignature {
                error: error.to_string(),
            })
    }

    fn get_verification_inputs<T>(
        &self,
        author: SuiAddress
    ) -> SuiResult<(Self::Sig, Self::PubKey)> {
        // Is this signature emitted by the expected author?
        let bytes = self.public_key_bytes();
        let pk = Self::PubKey::from_bytes(bytes)
            .map_err(|_| SuiError::InvalidSignature {
                error: format!("Invalid public key"),
            })?;

        let received_addr = SuiAddress::try_from(self.public_key_bytes())?;
        if received_addr != author {
            return Err(SuiError::IncorrectSigner {
                error: format!("Signature get_verification_inputs() failure. Author is {author}, received address is {received_addr}")
            });
        }

        // deserialize the signature
        let signature = Self::Sig::from_bytes(self.signature_bytes()).map_err(|err| {
            SuiError::InvalidSignature {
                error: err.to_string(),
            }
        })?;
        Ok((signature, pk))
    }

    const SIG_AND_FLAG_LENGTH: usize = Self::Sig::LENGTH + FLAG_LENGTH;

    fn _try_sign(kp: Self::KeyPair, msg: &[u8]) -> Result<Self, signature::Error> {
        let signature_bytes: Self::Sig = kp.try_sign(msg)?;
        let pk_bytes = kp.public().as_ref();
        let mut result_bytes: Vec<u8> = vec![0u8; Self::LENGTH];
        
        result_bytes[..FLAG_LENGTH].copy_from_slice(&Self::FLAG);
        result_bytes[FLAG_LENGTH..Self::SIG_AND_FLAG_LENGTH].copy_from_slice(&signature_bytes.as_ref());
        result_bytes[Self::SIG_AND_FLAG_LENGTH..].copy_from_slice(pk_bytes);

        Ok(Self(result_bytes))
    }
}
 

/// AuthoritySignInfoTrait is a trait used specifically for a few structs in messages.rs
/// to template on whether the struct is signed by an authority. We want to limit how
/// those structs can be instanted on, hence the sealed trait.
/// TODO: We could also add the aggregated signature as another impl of the trait.
///       This will make CertifiedTransaction also an instance of the same struct.
pub trait AuthoritySignInfoTrait: private::SealedAuthoritySignInfoTrait {}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct EmptySignInfo {}
impl AuthoritySignInfoTrait for EmptySignInfo {}

#[derive(Clone, Debug, Eq, Serialize, Deserialize)]
pub struct AuthoritySignInfo {
    pub epoch: EpochId,
    pub authority: AuthorityName,
    pub signature: AuthoritySignature,
}
impl AuthoritySignInfoTrait for AuthoritySignInfo {}

impl Hash for AuthoritySignInfo {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.epoch.hash(state);
        self.authority.hash(state);
    }
}

impl PartialEq for AuthoritySignInfo {
    fn eq(&self, other: &Self) -> bool {
        // We do not compare the signature, because there can be multiple
        // valid signatures for the same epoch and authority.
        self.epoch == other.epoch && self.authority == other.authority
    }
}

impl AuthoritySignInfo {
    pub fn add_to_verification_obligation(
        &self,
        committee: &Committee,
        obligation: &mut VerificationObligation<AggregateAuthoritySignature>,
        message_index: usize,
    ) -> SuiResult<()> {
        let weight = committee.weight(&self.authority);
        fp_ensure!(weight > 0, SuiError::UnknownSigner);

        obligation
            .public_keys
            .get_mut(message_index)
            .ok_or(SuiError::InvalidAddress)?
            .push(committee.public_key(&self.authority)?);
        obligation
            .signatures
            .get_mut(message_index)
            .ok_or(SuiError::InvalidAddress)?
            .add_signature(self.signature.clone())
            .map_err(|_| SuiError::InvalidSignature {
                error: "Invalid Signature".to_string(),
            })?;
        Ok(())
    }

    pub fn verify<T>(&self, data: &T, committee: &Committee) -> SuiResult<()>
    where
        T: Signable<Vec<u8>>,
    {
        let mut obligation = VerificationObligation::default();
        let idx = obligation.add_message(data);
        self.add_to_verification_obligation(committee, &mut obligation, idx)?;
        obligation.verify_all()?;
        Ok(())
    }
}

/// Represents at least a quorum (could be more) of authority signatures.
/// STRONG_THRESHOLD indicates whether to use the quorum threshold for quorum check.
/// When STRONG_THRESHOLD is true, the quorum is valid when the total stake is
/// at least the quorum threshold (2f+1) of the committee; when STRONG_THRESHOLD is false,
/// the quorum is valid when the total stake is at least the validity threshold (f+1) of
/// the committee.
#[serde_as]
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct AuthorityQuorumSignInfo<const STRONG_THRESHOLD: bool> {
    pub epoch: EpochId,
    #[schemars(with = "Base64")]
    pub signature: AggregateAuthoritySignature,
    #[schemars(with = "Base64")]
    #[serde_as(as = "SuiBitmap")]
    pub signers_map: RoaringBitmap,
}

pub type AuthorityStrongQuorumSignInfo = AuthorityQuorumSignInfo<true>;
pub type AuthorityWeakQuorumSignInfo = AuthorityQuorumSignInfo<false>;

// Note: if you meet an error due to this line it may be because you need an Eq implementation for `CertifiedTransaction`,
// or one of the structs that include it, i.e. `ConfirmationTransaction`, `TransactionInfoResponse` or `ObjectInfoResponse`.
//
// Please note that any such implementation must be agnostic to the exact set of signatures in the certificate, as
// clients are allowed to equivocate on the exact nature of valid certificates they send to the system. This assertion
// is a simple tool to make sure certificates are accounted for correctly - should you remove it, you're on your own to
// maintain the invariant that valid certificates with distinct signatures are equivalent, but yet-unchecked
// certificates that differ on signers aren't.
//
// see also https://github.com/MystenLabs/sui/issues/266
static_assertions::assert_not_impl_any!(AuthorityStrongQuorumSignInfo: Hash, Eq, PartialEq);
static_assertions::assert_not_impl_any!(AuthorityWeakQuorumSignInfo: Hash, Eq, PartialEq);

impl<const S: bool> AuthoritySignInfoTrait for AuthorityQuorumSignInfo<S> {}

impl<const STRONG_THRESHOLD: bool> AuthorityQuorumSignInfo<STRONG_THRESHOLD> {
    pub fn new(epoch: EpochId) -> Self {
        AuthorityQuorumSignInfo {
            epoch,
            signature: AggregateAuthoritySignature::default(),
            signers_map: RoaringBitmap::new(),
        }
    }

    pub fn new_with_signatures(
        epoch: EpochId,
        mut signatures: Vec<(AuthorityPublicKeyBytes, AuthoritySignature)>,
        committee: &Committee,
    ) -> SuiResult<Self> {
        let mut map = RoaringBitmap::new();
        signatures.sort_by_key(|(public_key, _)| *public_key);

        for (pk, _) in &signatures {
            map.insert(
                committee
                    .authority_index(pk)
                    .ok_or(SuiError::UnknownSigner)? as u32,
            );
        }
        let sigs: Vec<AuthoritySignature> = signatures.into_iter().map(|(_, sig)| sig).collect();

        Ok(AuthorityQuorumSignInfo {
            epoch,
            signature: AggregateAuthoritySignature::aggregate(sigs).map_err(|e| {
                SuiError::InvalidSignature {
                    error: e.to_string(),
                }
            })?,
            signers_map: map,
        })
    }

    pub fn authorities<'a>(
        &'a self,
        committee: &'a Committee,
    ) -> impl Iterator<Item = SuiResult<&AuthorityName>> {
        self.signers_map.iter().map(|i| {
            committee
                .authority_by_index(i)
                .ok_or(SuiError::InvalidAuthenticator)
        })
    }

    pub fn len(&self) -> u64 {
        self.signers_map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.signers_map.is_empty()
    }

    pub fn add_to_verification_obligation(
        &self,
        committee: &Committee,
        obligation: &mut VerificationObligation<AggregateAuthoritySignature>,
        message_index: usize,
    ) -> SuiResult<()> {
        // Check epoch
        fp_ensure!(
            self.epoch == committee.epoch(),
            SuiError::WrongEpoch {
                expected_epoch: committee.epoch()
            }
        );

        let mut weight = 0;

        // Create obligations for the committee signatures
        obligation
            .signatures
            .get_mut(message_index)
            .ok_or(SuiError::InvalidAuthenticator)?
            .add_aggregate(self.signature.clone())
            .map_err(|_| SuiError::InvalidSignature {
                error: "Signature Aggregation failed".to_string(),
            })?;

        let selected_public_keys = obligation
            .public_keys
            .get_mut(message_index)
            .ok_or(SuiError::InvalidAuthenticator)?;

        for authority_index in self.signers_map.iter() {
            let authority = committee
                .authority_by_index(authority_index)
                .ok_or(SuiError::UnknownSigner)?;

            // Update weight.
            let voting_rights = committee.weight(authority);
            fp_ensure!(voting_rights > 0, SuiError::UnknownSigner);
            weight += voting_rights;

            selected_public_keys.push(committee.public_key(authority)?);
        }

        let threshold = if STRONG_THRESHOLD {
            committee.quorum_threshold()
        } else {
            committee.validity_threshold()
        };
        fp_ensure!(weight >= threshold, SuiError::CertificateRequiresQuorum);

        Ok(())
    }

    pub fn verify<T>(&self, data: &T, committee: &Committee) -> SuiResult<()>
    where
        T: Signable<Vec<u8>>,
    {
        let mut obligation = VerificationObligation::default();
        let message_index = obligation.add_message(data);
        self.add_to_verification_obligation(committee, &mut obligation, message_index)?;
        obligation.verify_all()?;
        Ok(())
    }
}

mod private {
    pub trait SealedAuthoritySignInfoTrait {}
    impl SealedAuthoritySignInfoTrait for super::EmptySignInfo {}
    impl SealedAuthoritySignInfoTrait for super::AuthoritySignInfo {}
    impl<const S: bool> SealedAuthoritySignInfoTrait for super::AuthorityQuorumSignInfo<S> {}
}

/// Something that we know how to hash and sign.
pub trait Signable<W> {
    fn write(&self, writer: &mut W);
}
pub trait SignableBytes
where
    Self: Sized,
{
    fn from_signable_bytes(bytes: &[u8]) -> Result<Self, anyhow::Error>;
}
/// Activate the blanket implementation of `Signable` based on serde and BCS.
/// * We use `serde_name` to extract a seed from the name of structs and enums.
/// * We use `BCS` to generate canonical bytes suitable for hashing and signing.
pub trait BcsSignable: Serialize + serde::de::DeserializeOwned {}

impl<T, W> Signable<W> for T
where
    T: BcsSignable,
    W: std::io::Write,
{
    fn write(&self, writer: &mut W) {
        let name = serde_name::trace_name::<Self>().expect("Self must be a struct or an enum");
        // Note: This assumes that names never contain the separator `::`.
        write!(writer, "{}::", name).expect("Hasher should not fail");
        bcs::serialize_into(writer, &self).expect("Message serialization should not fail");
    }
}

impl<T> SignableBytes for T
where
    T: BcsSignable,
{
    fn from_signable_bytes(bytes: &[u8]) -> Result<Self, Error> {
        // Remove name tag before deserialization using BCS
        let name = serde_name::trace_name::<Self>().expect("Self must be a struct or an enum");
        let name_byte_len = format!("{}::", name).bytes().len();
        Ok(bcs::from_bytes(&bytes[name_byte_len..])?)
    }
}

pub fn sha3_hash<S: Signable<Sha3_256>>(signable: &S) -> [u8; 32] {
    let mut digest = Sha3_256::default();
    signable.write(&mut digest);
    let hash = digest.finalize();
    hash.into()
}

#[derive(Default)]
pub struct VerificationObligation<S>
where
    S: AggregateAuthenticator,
{
    pub messages: Vec<Vec<u8>>,
    pub signatures: Vec<S>,
    pub public_keys: Vec<Vec<S::PubKey>>,
}

impl<S: AggregateAuthenticator> VerificationObligation<S> {
    pub fn new() -> VerificationObligation<S> {
        VerificationObligation {
            ..Default::default()
        }
    }

    /// Add a new message to the list of messages to be verified.
    /// Returns the index of the message.
    pub fn add_message<T>(&mut self, message_value: &T) -> usize
    where
        T: Signable<Vec<u8>>,
    {
        let mut message = Vec::new();
        message_value.write(&mut message);

        self.signatures.push(S::default());
        self.public_keys.push(Vec::new());
        self.messages.push(message);
        self.messages.len() - 1
    }

    pub fn verify_all(self) -> SuiResult<()> {
        S::batch_verify(
            &self.signatures[..],
            &self.public_keys.iter().map(|x| &x[..]).collect::<Vec<_>>(),
            &self.messages.iter().map(|x| &x[..]).collect::<Vec<_>>()[..],
        )
        .map_err(|error| SuiError::InvalidSignature {
            error: format!("{error}"),
        })?;

        Ok(())
    }
}
