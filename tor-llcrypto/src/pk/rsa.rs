//! Re-exporting RSA implementations.
//!
//! This module can currently handle public keys and signature
//! verification as they work in the Tor directory protocol and
//! similar places.
//!
//! Currently, that means supporting validating PKCSv1
//! signatures, and encoding and decoding keys from DER.
use arrayref::array_ref;
use subtle::*;
use zeroize::Zeroize;

/// How many bytes are in an "RSA ID"?  (This is a legacy tor
/// concept, and refers to identifying a relay by a SHA1 digest
/// of its public key.)
pub const RSA_ID_LEN: usize = 20;

/// An identifier for a Tor relay, based on its legacy RSA
/// identity key.
#[derive(Clone, Zeroize, Debug)]
pub struct RSAIdentity {
    pub id: [u8; RSA_ID_LEN],
}

impl PartialEq<RSAIdentity> for RSAIdentity {
    fn eq(&self, rhs: &RSAIdentity) -> bool {
        self.id.ct_eq(&rhs.id).unwrap_u8() == 1
    }
}

impl Eq for RSAIdentity {}

impl RSAIdentity {
    pub fn as_bytes(&self) -> &[u8] {
        &self.id[..]
    }
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() == RSA_ID_LEN {
            Some(RSAIdentity {
                id: *array_ref![bytes, 0, RSA_ID_LEN],
            })
        } else {
            None
        }
    }
}

/// An RSA public key.
pub struct PublicKey(rsa::RSAPublicKey);
/// An RSA Private key.
pub struct PrivateKey(rsa::RSAPrivateKey);

impl PrivateKey {
    pub fn to_public_key(&self) -> PublicKey {
        PublicKey(self.0.to_public_key())
    }
    // ....
}
impl PublicKey {
    /// Return true iff the exponent for this key is the same
    /// number as 'e'.
    pub fn exponent_is(&self, e: u32) -> bool {
        use rsa::PublicKey;
        *self.0.e() == rsa::BigUint::new(vec![e])
    }
    /// Return the number of bits in the modulus for this key.
    pub fn bits(&self) -> usize {
        use rsa::PublicKey;
        self.0.n().bits()
    }
    /// Try to check a signature (as used in Tor.)  The signed hash
    /// should be in 'hashed', and the alleged signature in 'sig'.
    ///
    /// Tor uses RSA-PKCSv1 signatures, with hash algorithm OIDs
    /// omitted.
    pub fn verify(&self, hashed: &[u8], sig: &[u8]) -> rsa::errors::Result<()> {
        use rsa::PublicKey;
        self.0
            .verify::<rsa::hash::Hashes>(rsa::PaddingScheme::PKCS1v15, None, hashed, sig)
        // XXXX I don't want to expose rsa::errors::Result, really.
    }
    /// Decode an alleged DER byte string into a PublicKey. Return None
    /// if the DER string does not have a valid PublicKey.
    ///
    /// (Does not expect or allow an OID.)
    pub fn from_der(der: &[u8]) -> Option<Self> {
        // We can't use the rsa-der crate, since it expects to find the
        // key inside of a bitstring inside of another asn1 object.
        // Also it doesn't seem to check for negative values.
        let blocks = simple_asn1::from_der(der).ok()?;
        if blocks.len() != 1 {
            return None;
        }
        let block = &blocks[0];
        use simple_asn1::ASN1Block::*;
        let (n, e) = match block {
            Sequence(_, v) => match &v[..] {
                [Integer(_, n), Integer(_, e)] => (n, e),
                _ => return None,
            },
            _ => return None,
        };
        use num_traits::sign::Signed;
        if n.is_negative() || e.is_negative() {
            return None;
        }
        let (_, nbytes) = n.to_bytes_be();
        let (_, ebytes) = e.to_bytes_be();
        let pk = PublicKey(
            rsa::RSAPublicKey::new(
                rsa::BigUint::from_bytes_be(&nbytes),
                rsa::BigUint::from_bytes_be(&ebytes),
            )
            .ok()?,
        );

        // assert_eq!(der, &pk.to_der()[..]);

        Some(pk)
    }
    /// Encode this public key into the DER format as used by Tor.
    ///
    /// Does not attach an OID.
    pub fn to_der(&self) -> Vec<u8> {
        use simple_asn1::ASN1Block;
        // XXX do I really need both of these crates? rsa uses
        // bigint_dig, and simple_asn1 uses bigint.
        use num_bigint::{BigInt, Sign};
        use rsa::BigUint; // not the same as the one in num_bigint.
        use rsa::PublicKey;
        fn to_asn1_int(x: &BigUint) -> ASN1Block {
            let bytes = x.to_bytes_be();
            let bigint = BigInt::from_bytes_be(Sign::Plus, &bytes);
            ASN1Block::Integer(0, bigint)
        }

        let asn1 = ASN1Block::Sequence(0, vec![to_asn1_int(self.0.n()), to_asn1_int(self.0.e())]);
        simple_asn1::to_der(&asn1).unwrap()
    }

    /// Compute the RSAIdentity for this public key.
    pub fn to_rsa_identity(&self) -> RSAIdentity {
        use crate::d::Sha1;
        use crate::traits::Digest;
        let id = Sha1::digest(&self.to_der()).into();
        RSAIdentity { id }
    }
}
