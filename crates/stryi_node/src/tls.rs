use pem::Pem;
use pkcs8::{ObjectIdentifier, PrivateKeyInfo};
use pkcs8::der::Encode;
use pkcs8::spki::AlgorithmIdentifier;
use rcgen::{date_time_ymd, CertificateParams, DistinguishedName, KeyPair, PKCS_ED25519};
use rustls_pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
use stryi_network::Keypair; // Re-import of libp2p's Keypair  
use crate::error::StryiNodeError;

/// TLS identity of a Stryi node:
/// *one* certificate + its matching private key, generated from the
/// node’s libp2p Ed25519 key.
pub struct NodeTlsIdentity {
    /// PEM-encoded X.509 certificate (`-----BEGIN CERTIFICATE----- …`)
    pub cert_pem: String,

    /// PEM-encoded PKCS#8 private key (`-----BEGIN PRIVATE KEY----- …`)
    pub key_pem: String,
}

/// Wrap a 32-byte Ed25519 seed in a valid PKCS-8 OneAsymmetricKey.
fn pkcs8_from_seed(seed: &[u8; 32]) -> Result<PrivateKeyDer<'static>, pkcs8::Error> {
    // Generate inner - an octet string of `04 20` + <seed>
    let inner = pkcs8::der::asn1::OctetStringRef::new(seed)?.to_der()?;

    // Outer structure
    let ed25519_oid: ObjectIdentifier = ObjectIdentifier::new("1.3.101.112").unwrap();

    let info = PrivateKeyInfo {
        algorithm: AlgorithmIdentifier { oid: ed25519_oid, parameters: None },
        private_key: &inner,
        public_key: None,
    };

    let der = info.to_der()?;
    Ok(PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(der)))
}


/// Helper function that generates x.509 cert based on provided peer key
/// Returns cert and private key both in PEM format
pub fn cert_and_key_from_peer(
    peer_key: &Keypair,
    sans: &[&str],
) -> Result<NodeTlsIdentity, StryiNodeError> {
    // 1. Convert libp2p key -> seed -> PrivateKeyDer -> rcgen::key_pair::KeyPair
    let seed: [u8; 32] = peer_key
        .clone()
        .try_into_ed25519()
        .expect("peer key must be Ed25519")
        .secret()
        .as_ref()
        .try_into()
        .map_err(|e| StryiNodeError::other(e))?;


    let private_key_der = pkcs8_from_seed(&seed)?;

    let key_pair = KeyPair::from_der_and_sign_algo(&private_key_der, &PKCS_ED25519).expect("It will never happen anyways");

    // 2. Build params
    let mut params = CertificateParams::new(sans.iter().map(|s| s.to_string()).collect::<Vec<_>>())
        .map_err(|e| StryiNodeError::Other(e.to_string()))?;
    params.distinguished_name = DistinguishedName::new();
    params.not_before = date_time_ymd(1975, 1, 1);
    params.not_after  = date_time_ymd(4096, 1, 1);
    params.key_usages = vec![rcgen::KeyUsagePurpose::DigitalSignature];
    params.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ServerAuth];

    // 3. Self-sign & serialize
    let cert = params.self_signed(&key_pair).map_err(|e| StryiNodeError::other(e))?;


    let cert_pem = cert.pem();

    
    let der = key_pair.serialize_der(); // Vec<u8> 
    let key_pem = pem::encode(&Pem::new("PRIVATE KEY", der)); // String
    
    
    Ok(NodeTlsIdentity { cert_pem, key_pem })
}

#[cfg(test)]
mod tests {
    use stryi_network::Keypair;
    use super::{cert_and_key_from_peer, NodeTlsIdentity};
    use x509_parser::{parse_x509_certificate, prelude::*};

    #[test]
    fn peer_cert_contains_same_public_key() {
        //  generate a random Ed25519 peer key
        let peer_key = Keypair::generate_ed25519();
        let libp2p_pk = peer_key.public(); // libp2p public key bytes

        // call the helper under test 
        let NodeTlsIdentity { cert_pem, key_pem} = cert_and_key_from_peer(&peer_key, &["stryi.service"]).unwrap();

        
        println!("{cert_pem}");
        println!("{key_pem}");
        
        
        // basic sanity: PEM headers
        assert!(cert_pem.starts_with("-----BEGIN CERTIFICATE-----"));
        assert!(key_pem.starts_with("-----BEGIN PRIVATE KEY-----"));

        // parse certificate DER
        // strip PEM into DER
        let der_bytes = parse_x509_pem(&cert_pem.as_bytes()).unwrap().1.contents;


        // X.509 parse
        let (_, cert) = parse_x509_certificate(&der_bytes).expect("valid x509");

        // extract the public key from the certificate
        let spki = cert.tbs_certificate.subject_pki;
        // For Ed25519, the SubjectPublicKey is a 32-byte raw key
        let cert_pk_bytes = &spki.subject_public_key.data[0..32];

        // libp2p public key in raw form (Ed25519 = 32 bytes)
        let libp2p_pk_bytes = &libp2p_pk.try_into_ed25519().unwrap().to_bytes();

        assert_eq!(
            cert_pk_bytes,
            libp2p_pk_bytes,
            "certificate public key differs from libp2p peer public key"
        );
    }
}
