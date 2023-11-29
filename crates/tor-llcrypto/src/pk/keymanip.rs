//! Key manipulation functions for use with public keys.
//!
//! Tor does some interesting and not-standard things with its
//! curve25519 and ed25519 keys, for several reasons.
//!
//! In order to prove ownership of a curve25519 private key, Tor
//! converts it into an ed25519 key, and then uses that ed25519 key to
//! sign its identity key.  We implement this conversion with
//! [`convert_curve25519_to_ed25519_public`] and
//! [`convert_curve25519_to_ed25519_private`].
//!
//! In Tor's v3 onion service design, Tor uses a _key blinding_
//! algorithm to derive a publicly known Ed25519 key from a different
//! Ed25519 key used as the .onion address.  This algorithm allows
//! directories to validate the signatures on onion service
//! descriptors, without knowing which services they represent.  We
//! implement this blinding operation via [`blind_pubkey`].
//!
//! ## TODO
//!
//! Recommend more standardized ways to do these things.

// Ideally there would be a feature that we would use in the CI, rather than this ad-hoc list.
#![cfg_attr(
    not(all(test, feature = "hsv3-service", feature = "relay")),
    allow(unused_imports)
)]

use crate::{d, pk};
use digest::Digest;
use thiserror::Error;

use curve25519_dalek::scalar::Scalar;
// TODO DALEK: We are renaming a few types here to maintain consistency with our
// old names.  Likely, we should deprecate a few of these renamings. Possibly
// all.
//
// TODO DALEK: ALso, it's likely we should remove hazmat::ExpandedSecretKey from
// our exports, and instead just use ExpandedKeypair.
pub use ed25519_dalek::{
    hazmat::ExpandedSecretKey, Signature, SigningKey as Keypair, VerifyingKey as PublicKey,
};
pub use pk::ed25519::ExpandedKeypair;

/// Convert a curve25519 public key (with sign bit) to an ed25519
/// public key, for use in ntor key cross-certification.
///
/// Note that this formula is not standardized; don't use
/// it for anything besides cross-certification.
pub fn convert_curve25519_to_ed25519_public(
    pubkey: &pk::curve25519::PublicKey,
    signbit: u8,
) -> Option<pk::ed25519::PublicKey> {
    use curve25519_dalek::montgomery::MontgomeryPoint;

    let point = MontgomeryPoint(*pubkey.as_bytes());
    let edpoint = point.to_edwards(signbit)?;

    // TODO: This is inefficient; we shouldn't have to re-compress
    // this point to get the public key we wanted.  But there's no way
    // with the current API that I can to construct an ed25519 public
    // key from a compressed point.
    let compressed_y = edpoint.compress();
    pk::ed25519::PublicKey::from_bytes(compressed_y.as_bytes()).ok()
}

/// Convert a curve25519 private key to an ed25519 private key (and
/// give a sign bit) to use with it, for use in ntor key cross-certification.
///
/// Note that this formula is not standardized; don't use
/// it for anything besides cross-certification.
///
/// *NEVER* use these keys to sign inputs that may be generated by an
/// attacker.
///
/// # Panics
///
/// If the `debug_assertions` feature is enabled, this function will
/// double-check that the key it is about to return is the right
/// private key for the public key returned by
/// `convert_curve25519_to_ed25519_public`.
///
/// This panic should be impossible unless there are implementation
/// bugs.
#[cfg(any(test, feature = "cvt-x25519"))]
pub fn convert_curve25519_to_ed25519_private(
    privkey: &pk::curve25519::StaticSecret,
) -> Option<(pk::ed25519::ExpandedKeypair, u8)> {
    use crate::d::Sha512;
    use zeroize::Zeroizing;

    let h = Sha512::new()
        .chain_update(privkey.to_bytes())
        .chain_update(&b"Derive high part of ed25519 key from curve25519 key\0"[..])
        .finalize();

    let mut bytes = Zeroizing::new([0_u8; 64]);
    bytes[0..32].clone_from_slice(&privkey.to_bytes());
    bytes[32..64].clone_from_slice(&h[0..32]);

    let secret = ed25519_dalek::hazmat::ExpandedSecretKey::from_bytes(&bytes);
    let public: pk::ed25519::PublicKey = (&secret).into();
    let signbit = public.as_bytes()[31] >> 7;

    #[cfg(debug_assertions)]
    {
        let curve_pubkey1 = pk::curve25519::PublicKey::from(privkey);
        let ed_pubkey1 = convert_curve25519_to_ed25519_public(&curve_pubkey1, signbit)?;
        assert_eq!(ed_pubkey1, public);
    }

    Some((pk::ed25519::ExpandedKeypair { public, secret }, signbit))
}

/// Convert an ed25519 private key to a curve25519 private key.
///
/// This creates a curve25519 key as described in section-5.1.5 of RFC8032: the bytes of the secret
/// part of `keypair` are hashed using SHA-512, and the result is clamped (the first 3 bits of the
/// first byte are cleared, the highest bit of the last byte is cleared, the second highest bit of
/// the last byte is set).
///
/// Note: Using the same keypair for multiple purposes (such as key-exchange and signing) is
/// considered bad practice. Don't use this function unless you know what you're doing.
/// See [On using the same key pair for Ed25519 and an X25519 based
/// KEM](https://eprint.iacr.org/2021/509.pdf).
///
/// This function is needed by the `ArtiNativeKeystore` from `tor-keymgr` to convert ed25519
/// private keys to x25519. This is because `ArtiNativeKeystore` stores x25519 private keys as
/// ssh-ed25519 OpenSSH keys. Other similar use cases are also valid.
///
/// It's important to note that converting a private key from ed25519 -> curve25519 -> ed25519 will
/// yield an [`ExpandedKeypair`] that is _not_ identical to the
/// expanded version of the original [`Keypair`]: the lower halves (the keys) of
/// the expanded key pairs will be the same, but their upper halves (the nonces) will be different.
///
/// # Panics
///
/// If the `debug_assertions` feature is enabled, this function will double-check that the key it
/// is about to return is clamped.
///
/// This panic should be impossible unless we have upgraded x25519-dalek without auditing this
/// function.
#[cfg(any(test, feature = "cvt-x25519"))]
#[deprecated(
    since = "0.6.0",
    note = "ed25519_to_curve25519 conversion is unused, and no longer supported."
)]
pub fn convert_ed25519_to_curve25519_private(
    keypair: &pk::ed25519::Keypair,
) -> pk::curve25519::StaticSecret {
    use crate::d::Sha512;
    use zeroize::{Zeroize as _, Zeroizing};

    // Generate the key according to section-5.1.5 of rfc8032
    let h = Sha512::digest(keypair.to_bytes());

    let mut bytes = Zeroizing::new([0_u8; 32]);
    bytes.clone_from_slice(&h[0..32]);

    // Clamp the bytes.  We do not necessarily have to do this, since
    // x25519-dalek will handle clamping before it does any computation,  but we
    // want to make sure that the StaticSecret we generate is in the usual
    // format.
    let mut bytes = curve25519_dalek::scalar::clamp_integer(*bytes);

    let secret = pk::curve25519::StaticSecret::from(bytes);
    bytes.zeroize();

    secret
}

/// An error occurred during a key-blinding operation.
#[derive(Error, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum BlindingError {
    /// A bad public key was provided for blinding
    #[error("Public key was invalid")]
    BadPubkey,
    /// Dalek failed the scalar multiplication
    #[error("Key blinding failed")]
    BlindingFailed,
}

// Convert this dalek error to a BlindingError
impl From<ed25519_dalek::SignatureError> for BlindingError {
    fn from(_: ed25519_dalek::SignatureError) -> BlindingError {
        BlindingError::BlindingFailed
    }
}

/// Helper: clamp a blinding factor and use it to compute a blinding factor.
///
/// Described in part of rend-spec-v3 A.2.
///
/// This is a common step for public-key and private-key blinding.
#[cfg(any(feature = "hsv3-client", feature = "hsv3-service"))]
fn clamp_blinding_factor(h: [u8; 32]) -> Scalar {
    // Transform it into a scalar so that we can do scalar mult.
    Scalar::from_bytes_mod_order(curve25519_dalek::scalar::clamp_integer(h))
}

/// Blind the ed25519 public key `pk` using the blinding factor
/// `h`, and return the blinded public key.
///
/// This algorithm is described in `rend-spec-v3.txt`, section A.2.
/// In the terminology of that section, the value `pk` corresponds to
/// `A`, and
/// `h` is the value `h = H(...)`, before clamping.
///
/// Note that the approach used to clamp `h` to a scalar means
/// that different possible values for `h` may yield the same
/// output for a given `pk`.  This and other limitations make this
/// function unsuitable for use outside the context of
/// `rend-spec-v3.txt` without careful analysis.
///
/// # Errors
///
/// This function can fail if the input is not actually a valid
/// Ed25519 public key.
///
/// # Availability
///
/// This function is only available when the `hsv3-client` feature is enabled.
#[cfg(feature = "hsv3-client")]
pub fn blind_pubkey(pk: &PublicKey, h: [u8; 32]) -> Result<PublicKey, BlindingError> {
    use curve25519_dalek::edwards::CompressedEdwardsY;

    let blinding_factor = clamp_blinding_factor(h);

    // Convert the public key to a point on the curve
    let pubkey_point = CompressedEdwardsY(pk.to_bytes())
        .decompress()
        .ok_or(BlindingError::BadPubkey)?;

    // Do the scalar multiplication and get a point back
    let blinded_pubkey_point = (blinding_factor * pubkey_point).compress();
    // Turn the point back into bytes and return it
    Ok(PublicKey::from_bytes(&blinded_pubkey_point.0)?)
}

/// Blind the ed25519 secret key `sk` using the blinding factor `h`, and
/// return the blinded secret key.
///
/// This algorithm is described in `rend-spec-v3.txt`, section A.2.
/// `h` is the value `h = H(...)`, before clamping.
///
/// Note that the approach used to clamp `h` to a scalar means that
/// different possible values for `h` may yield the same output for a given
/// `pk`.  This and other limitations make this function unsuitable for use
/// outside the context of `rend-spec-v3.txt` without careful analysis.
///
/// # Errors
///
/// This function can fail if the input is not actually a valid Ed25519 secret
/// key.
///
/// # Availability
///
/// This function is only available when the `hsv3-client` feature is enabled.
#[cfg(feature = "hsv3-service")]
pub fn blind_keypair(
    keypair: &ExpandedKeypair,
    h: [u8; 32],
) -> Result<ExpandedKeypair, BlindingError> {
    use zeroize::Zeroizing;

    /// Fixed string specified in rend-spec-v3.txt, used for blinding the
    /// original nonce.  (Technically, any string would do, but this one keeps
    /// implementations consistent.)
    const RH_BLIND_STRING: &[u8] = b"Derive temporary signing key hash input";

    let blinding_factor = clamp_blinding_factor(h);

    let blinded_secret_scalar = keypair.secret.scalar * blinding_factor;

    let blinded_secret_hash_prefix = {
        let mut h = d::Sha512::new();
        h.update(RH_BLIND_STRING);
        h.update(keypair.secret.hash_prefix);
        let mut d = Zeroizing::new([0_u8; 64]);
        h.finalize_into(
            d.as_mut()
                .try_into()
                .expect("64-byte array not 64 bytes long!?"),
        );
        d[0..32].try_into().expect("slice cast failed")
    };

    let secret = ExpandedSecretKey {
        scalar: blinded_secret_scalar,
        hash_prefix: blinded_secret_hash_prefix,
    };
    let public = PublicKey::from(&secret);

    #[cfg(debug_assertions)]
    {
        // Make sure that the public key that derives from our
        // blinded key is the same as the key that we get when we re-blind the
        // public key.
        let public2 = blind_pubkey(&keypair.public, h)?;
        debug_assert_eq!(public, public2);
    }

    Ok(ExpandedKeypair { secret, public })
}

#[cfg(test)]
mod tests {
    // @@ begin test lint list maintained by maint/add_warning @@
    #![allow(clippy::bool_assert_comparison)]
    #![allow(clippy::clone_on_copy)]
    #![allow(clippy::dbg_macro)]
    #![allow(clippy::print_stderr)]
    #![allow(clippy::print_stdout)]
    #![allow(clippy::single_char_pattern)]
    #![allow(clippy::unwrap_used)]
    #![allow(clippy::unchecked_duration_subtraction)]
    #![allow(clippy::useless_vec)]
    #![allow(clippy::needless_pass_by_value)]
    //! <!-- @@ end test lint list maintained by maint/add_warning @@ -->
    use super::*;

    #[test]
    fn curve_to_ed_compatible() {
        use crate::pk::{curve25519, ed25519};
        use ed25519_dalek::Verifier;
        use tor_basic_utils::test_rng::testing_rng;

        let rng = testing_rng();

        let curve_sk = curve25519::StaticSecret::random_from_rng(rng);
        let curve_pk = curve25519::PublicKey::from(&curve_sk);

        let (ed_kp, signbit) = convert_curve25519_to_ed25519_private(&curve_sk).unwrap();
        let ed_sk = &ed_kp.secret;
        let ed_pk0 = ed_kp.public;
        let ed_pk1: ed25519::PublicKey = (ed_sk).into();
        let ed_pk2 = convert_curve25519_to_ed25519_public(&curve_pk, signbit).unwrap();

        let msg = b"tis the gift to be simple";
        let sig1 = ed_kp.sign(&msg[..]);
        assert!(ed_pk1.verify(&msg[..], &sig1).is_ok());
        assert!(ed_pk2.verify(&msg[..], &sig1).is_ok());

        assert_eq!(ed_pk1, ed_pk0);
        assert_eq!(ed_pk1, ed_pk2);
    }

    #[test]
    fn ed_to_curve_compatible() {
        use crate::pk::{curve25519, ed25519};
        use ed25519_dalek::Verifier;
        use tor_basic_utils::test_rng::testing_rng;

        let mut rng = testing_rng();
        let ed_kp = ed25519::Keypair::generate(&mut rng);
        let ed_ekp1 = ExpandedKeypair::from(&ed_kp);

        #[allow(deprecated)]
        let curve_sk = convert_ed25519_to_curve25519_private(&ed_kp);
        let curve_pk = curve25519::PublicKey::from(&curve_sk);

        let (ed_ekp2, signbit) = convert_curve25519_to_ed25519_private(&curve_sk).unwrap();
        let ed_pk2 = convert_curve25519_to_ed25519_public(&curve_pk, signbit).unwrap();

        assert_eq!(ed_ekp1.public, ed_ekp2.public);
        assert_eq!(ed_ekp2.public, ed_pk2);
        assert_eq!(ed_ekp1.secret.scalar, ed_ekp2.secret.scalar);

        // Make sure the 2 secret keys are the same. Note: we only look at the
        // scalar part of the (expanded) key, not the hash prefix.
        assert_eq!(ed_ekp1.secret.scalar, ed_ekp2.secret.scalar);

        let msg = b"tis the gift to be simple";

        for kp in &[&ed_ekp1, &ed_ekp2] {
            let sig = kp.sign(&msg[..]);
            assert!(ed_ekp1.public.verify(&msg[..], &sig).is_ok());
            assert!(ed_ekp2.public.verify(&msg[..], &sig).is_ok());
        }
    }

    #[test]
    #[cfg(all(feature = "hsv3-client", feature = "hsv3-service"))]
    fn blinding() {
        // Test the ed25519 blinding function.
        //
        // These test vectors are from our ed25519 implementation and related
        // functions. These were automatically generated by the
        // ed25519_exts_ref.py script in little-t-tor and they are also used by
        // little-t-tor and onionbalance:
        use ed25519_dalek::Verifier;

        let seckeys = vec![
            b"26c76712d89d906e6672dafa614c42e5cb1caac8c6568e4d2493087db51f0d36",
            b"fba7a5366b5cb98c2667a18783f5cf8f4f8d1a2ce939ad22a6e685edde85128d",
            b"67e3aa7a14fac8445d15e45e38a523481a69ae35513c9e4143eb1c2196729a0e",
            b"d51385942033a76dc17f089a59e6a5a7fe80d9c526ae8ddd8c3a506b99d3d0a6",
            b"5c8eac469bb3f1b85bc7cd893f52dc42a9ab66f1b02b5ce6a68e9b175d3bb433",
            b"eda433d483059b6d1ff8b7cfbd0fe406bfb23722c8f3c8252629284573b61b86",
            b"4377c40431c30883c5fbd9bc92ae48d1ed8a47b81d13806beac5351739b5533d",
            b"c6bbcce615839756aed2cc78b1de13884dd3618f48367a17597a16c1cd7a290b",
            b"c6bbcce615839756aed2cc78b1de13884dd3618f48367a17597a16c1cd7a290b",
            b"c6bbcce615839756aed2cc78b1de13884dd3618f48367a17597a16c1cd7a290b",
        ];
        let expanded_seckeys = vec![
            b"c0a4de23cc64392d85aa1da82b3defddbea946d13bb053bf8489fa9296281f495022f1f7ec0dcf52f07d4c7965c4eaed121d5d88d0a8ff546b06116a20e97755",
            b"18a8a69a06790dac778e882f7e868baacfa12521a5c058f5194f3a729184514a2a656fe7799c3e41f43d756da8d9cd47a061316cfe6147e23ea2f90d1ca45f30",
            b"58d84f8862d2ecfa30eb491a81c36d05b574310ea69dae18ecb57e992a896656b982187ee96c15bf4caeeab2d0b0ae4cd0b8d17470fc7efa98bb26428f4ef36d",
            b"50702d20b3550c6e16033db5ad4fba16436f1ecc7485be6af62b0732ceb5d173c47ccd9d044b6ea99dd99256adcc9c62191be194e7cb1a5b58ddcec85d876a2b",
            b"7077464c864c2ed5ed21c9916dc3b3ba6256f8b742fec67658d8d233dadc8d5a7a82c371083cc86892c2c8782dda2a09b6baf016aec51b689183ae59ce932ff2",
            b"8883c1387a6c86fc0bd7b9f157b4e4cd83f6885bf55e2706d2235d4527a2f05311a3595953282e436df0349e1bb313a19b3ddbf7a7b91ecce8a2c34abadb38b3",
            b"186791ac8d03a3ac8efed6ac360467edd5a3bed2d02b3be713ddd5be53b3287ee37436e5fd7ac43794394507ad440ecfdf59c4c255f19b768a273109e06d7d8e",
            b"b003077c1e52a62308eef7950b2d532e1d4a7eea50ad22d8ac11b892851f1c40ffb9c9ff8dcd0c6c233f665a2e176324d92416bfcfcd1f787424c0c667452d86",
            b"b003077c1e52a62308eef7950b2d532e1d4a7eea50ad22d8ac11b892851f1c40ffb9c9ff8dcd0c6c233f665a2e176324d92416bfcfcd1f787424c0c667452d86",
            b"b003077c1e52a62308eef7950b2d532e1d4a7eea50ad22d8ac11b892851f1c40ffb9c9ff8dcd0c6c233f665a2e176324d92416bfcfcd1f787424c0c667452d86",
        ];

        let pubkeys = vec![
            b"c2247870536a192d142d056abefca68d6193158e7c1a59c1654c954eccaff894",
            b"1519a3b15816a1aafab0b213892026ebf5c0dc232c58b21088d88cb90e9b940d",
            b"081faa81992e360ea22c06af1aba096e7a73f1c665bc8b3e4e531c46455fd1dd",
            b"73cfa1189a723aad7966137cbffa35140bb40d7e16eae4c40b79b5f0360dd65a",
            b"66c1a77104d86461b6f98f73acf3cd229c80624495d2d74d6fda1e940080a96b",
            b"d21c294db0e64cb2d8976625786ede1d9754186ae8197a64d72f68c792eecc19",
            b"c4d58b4cf85a348ff3d410dd936fa460c4f18da962c01b1963792b9dcc8a6ea6",
            b"95126f14d86494020665face03f2d42ee2b312a85bc729903eb17522954a1c4a",
            b"95126f14d86494020665face03f2d42ee2b312a85bc729903eb17522954a1c4a",
            b"95126f14d86494020665face03f2d42ee2b312a85bc729903eb17522954a1c4a",
        ];
        let params = vec![
            "54a513898b471d1d448a2f3c55c1de2c0ef718c447b04497eeb999ed32027823",
            "831e9b5325b5d31b7ae6197e9c7a7baf2ec361e08248bce055908971047a2347",
            "ac78a1d46faf3bfbbdc5af5f053dc6dc9023ed78236bec1760dadfd0b2603760",
            "f9c84dc0ac31571507993df94da1b3d28684a12ad14e67d0a068aba5c53019fc",
            "b1fe79d1dec9bc108df69f6612c72812755751f21ecc5af99663b30be8b9081f",
            "81f1512b63ab5fb5c1711a4ec83d379c420574aedffa8c3368e1c3989a3a0084",
            "97f45142597c473a4b0e9a12d64561133ad9e1155fe5a9807fe6af8a93557818",
            "3f44f6a5a92cde816635dfc12ade70539871078d2ff097278be2a555c9859cd0",
            "0000000000000000000000000000000000000000000000000000000000000000",
            "1111111111111111111111111111111111111111111111111111111111111111",
        ];
        let blinded_pubkeys = vec![
            "1fc1fa4465bd9d4956fdbdc9d3acb3c7019bb8d5606b951c2e1dfe0b42eaeb41",
            "1cbbd4a88ce8f165447f159d9f628ada18674158c4f7c5ead44ce8eb0fa6eb7e",
            "c5419ad133ffde7e0ac882055d942f582054132b092de377d587435722deb028",
            "3e08d0dc291066272e313014bfac4d39ad84aa93c038478a58011f431648105f",
            "59381f06acb6bf1389ba305f70874eed3e0f2ab57cdb7bc69ed59a9b8899ff4d",
            "2b946a484344eb1c17c89dd8b04196a84f3b7222c876a07a4cece85f676f87d9",
            "c6b585129b135f8769df2eba987e76e089e80ba3a2a6729134d3b28008ac098e",
            "0eefdc795b59cabbc194c6174e34ba9451e8355108520554ec285acabebb34ac",
            "312404d06a0a9de489904b18d5233e83a50b225977fa8734f2c897a73c067952",
            "952a908a4a9e0e5176a2549f8f328955aca6817a9fdc59e3acec5dec50838108",
        ];
        let blinded_seckeys = vec![
            "293c3acff4e902f6f63ddc5d5caa2a57e771db4f24de65d4c28df3232f47fa01171d43f24e3f53e70ec7ac280044ac77d4942dee5d6807118a59bdf3ee647e89",
            "38b88f9f9440358da544504ee152fb475528f7c51c285bd1c68b14ade8e29a07b8ceff20dfcf53eb52b891fc078c934efbf0353af7242e7dc51bb32a093afa29",
            "4d03ce16a3f3249846aac9de0a0075061495c3b027248eeee47da4ddbaf9e0049217f52e92797462bd890fc274672e05c98f2c82970d640084781334aae0f940",
            "51d7db01aaa0d937a9fd7c8c7381445a14d8fa61f43347af5460d7cd8fda9904509ecee77082ce088f7c19d5a00e955eeef8df6fa41686abc1030c2d76807733",
            "1f76cab834e222bd2546efa7e073425680ab88df186ff41327d3e40770129b00b57b95a440570659a440a3e4771465022a8e67af86bdf2d0990c54e7bb87ff9a",
            "c23588c23ee76093419d07b27c6df5922a03ac58f96c53671456a7d1bdbf560ec492fc87d5ec2a1b185ca5a40541fdef0b1e128fd5c2380c888bfa924711bcab",
            "3ed249c6932d076e1a2f6916975914b14e8c739da00992358b8f37d3e790650691b4768f8e556d78f4bdcb9a13b6f6066fe81d3134ae965dc48cd0785b3af2b8",
            "288cbfd923cb286d48c084555b5bdd06c05e92fb81acdb45271367f57515380e053d9c00c81e1331c06ab50087be8cfc7dc11691b132614474f1aa9c2503cccd",
            "e5cd03eb4cc456e11bc36724b558873df0045729b22d8b748360067a7770ac02053d9c00c81e1331c06ab50087be8cfc7dc11691b132614474f1aa9c2503cccd",
            "2cf7ed8b163f5af960d2fc62e1883aa422a6090736b4f18a5456ddcaf78ede0c053d9c00c81e1331c06ab50087be8cfc7dc11691b132614474f1aa9c2503cccd",
        ];

        for i in 0..pubkeys.len() {
            let sk: [u8; 32] = hex::decode(seckeys[i]).unwrap().try_into().unwrap();
            let esk = ExpandedSecretKey::from(&sk);
            let kp = Keypair::from(&sk);

            let esk_bytes_from_c_tor = hex::decode(expanded_seckeys[i]).unwrap();
            // Because of the differences in how we calculate the scalar, we
            // don't get the same _representation_ of the scalar as we did with
            // the C tor implementation.
            //
            // Therefore we have to do through this silliness to check our result.
            let c_scalar =
                Scalar::from_bytes_mod_order(esk_bytes_from_c_tor[0..32].try_into().unwrap());
            assert_eq!(c_scalar, esk.scalar);
            assert_eq!(
                hex::encode(esk.hash_prefix),
                hex::encode(&esk_bytes_from_c_tor[32..])
            );

            let public = (&esk).into();
            let kp_in = ExpandedKeypair {
                secret: esk,
                public,
            };

            let pk =
                PublicKey::from_bytes(&hex::decode(pubkeys[i]).unwrap()[..].try_into().unwrap())
                    .unwrap();
            assert_eq!(pk, PublicKey::from(&kp));

            let param = hex::decode(params[i]).unwrap().try_into().unwrap();
            // Blind the secret key, and make sure that the result is expected.
            let blinded_kp = blind_keypair(&kp_in, param).unwrap();
            assert_eq!(
                hex::encode(blinded_kp.to_secret_key_bytes()),
                blinded_seckeys[i]
            );

            // Make sure that the secret key can be encoded and decoded.
            {
                let blinded_kp2 =
                    ExpandedKeypair::from_secret_key_bytes(blinded_kp.to_secret_key_bytes())
                        .unwrap();
                assert_eq!(blinded_kp2.public, blinded_kp.public);
                assert_eq!(blinded_kp2.secret.scalar, blinded_kp.secret.scalar);
                assert_eq!(
                    blinded_kp2.secret.hash_prefix,
                    blinded_kp.secret.hash_prefix
                );
            }

            let blinded_pk = blind_pubkey(&pk, param).unwrap();

            // Make sure blinded pk is as expected.
            assert_eq!(hex::encode(blinded_pk.to_bytes()), blinded_pubkeys[i]);

            // Make sure that signature made with blinded sk is validated by
            // blinded pk.
            let sig = blinded_kp.sign(b"hello world");
            blinded_pk.verify(b"hello world", &sig).unwrap();

            let blinded_sk_scalar = blinded_kp.secret.scalar;
            let pk2 = blinded_sk_scalar * curve25519_dalek::constants::ED25519_BASEPOINT_POINT;
            let pk2 = pk2.compress();
            assert_eq!(pk2.as_bytes(), blinded_pk.as_bytes());
        }
    }
}
