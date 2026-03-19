//! RequestResponse-based custom behavior for collecting up-to-date information about the peer's services

use crate::ed25519::PublicKey;
use crate::{Keypair, Multiaddr, PeerId, StryiEvent, StryiNetworkError};
use bincode::config::standard;
use libp2p::core::SignedEnvelope;
use libp2p::identity;
use libp2p::request_response::Event as ReqRespEvent;
use libp2p::request_response::cbor::Behaviour as RequestResponseBehaviour;
use serde::de::Error as SerdeError;
use serde::de::Visitor;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;

/// Requests enum for ServicesInfo api
#[derive(Serialize, Deserialize, Clone, Debug)]
pub enum ServicesInfoRequest {
    ListServices,
    PushServices { services: Vec<SignedServiceRecord> },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServicesResponse {
    pub(crate) services: Vec<SignedServiceRecord>,
}

/// Transport security metadata advertised for a service endpoint.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum ServiceTransportSecurity {
    /// No transport-level protection.
    None,

    /// TLS is required and clients should pin the advertised PEM certificate.
    TlsServerCert { cert_pem: String },
}

/// ServiceInfo defines information we can gather about service(like gRPC api, json-rpc, etc.) which is running on some node/peer.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ServiceRecord {
    /// Address of this service
    address: Multiaddr,

    /// PeerId of the owner of the service
    owner: PeerId,

    /// The kind of service e.g. "grpc-sync", "http", etc
    kind: String,

    /// Version of service to define if the service is compatible with this node version.
    version: u32,

    /// Transport security configuration for the advertised endpoint.
    transport_security: ServiceTransportSecurity,
}

impl ServiceRecord {
    /// Create a new instance of ServiceRecord
    pub fn new(
        address: Multiaddr,
        owner: PeerId,
        kind: String,
        version: u32,
        transport_security: ServiceTransportSecurity,
    ) -> Self {
        Self {
            address,
            owner,
            kind,
            version,
            transport_security,
        }
    }

    /// Returns the address of the service.
    pub fn address(&self) -> &Multiaddr {
        &self.address
    }

    /// Returns PeerId of this record if it can convert string value to true PeerId instance
    /// Otherwise, returns error
    pub fn owner(&self) -> PeerId {
        self.owner
    }

    /// Returns the kind of the service.
    pub fn kind(&self) -> &str {
        &self.kind
    }

    /// Returns the version of the service.
    pub fn version(&self) -> u32 {
        self.version
    }

    /// Returns transport security metadata for the service endpoint.
    pub fn transport_security(&self) -> &ServiceTransportSecurity {
        &self.transport_security
    }
}

const SIGNED_SERVICE_RECORD_DOMAIN: &str = "stryichain.service";
const SIGNED_SERVICE_PAYLOAD_TYPE: &[u8] = b"\x71stryichain/service";

/// Signed transport payload for exchanging service records between peers.
#[derive(Clone, Debug)]
pub struct SignedServiceRecord {
    inner: SignedEnvelope,
}

impl SignedServiceRecord {
    /// Sign `ServiceRecord` with the node's ed25519 key.
    pub fn sign(
        peer_keypair: identity::ed25519::Keypair,
        service_record: ServiceRecord,
    ) -> Result<Self, StryiNetworkError> {
        let bincode_payload = bincode::serde::encode_to_vec(service_record, standard())
            .map_err(|e| StryiNetworkError::other(format!("bincode: {e}")))?;

        let kp: Keypair = peer_keypair.clone().into();

        let envelope = SignedEnvelope::new(
            &kp,
            SIGNED_SERVICE_RECORD_DOMAIN.to_string(),
            SIGNED_SERVICE_PAYLOAD_TYPE.to_vec(),
            bincode_payload,
        )
        .map_err(StryiNetworkError::SigningError)?;

        Ok(SignedServiceRecord { inner: envelope })
    }

    /// Returns `ServiceRecord` if:
    ///   - signature is valid,
    ///   - domain matches,
    ///   - payload_type matches,
    ///   - signing key equals the expected peer key.
    pub fn verify_and_decode(
        &self,
        expected_pk: &PublicKey,
    ) -> Result<ServiceRecord, StryiNetworkError> {
        let (payload, signing_key) = self
            .inner
            .payload_and_signing_key(
                SIGNED_SERVICE_RECORD_DOMAIN.to_string(),
                SIGNED_SERVICE_PAYLOAD_TYPE,
            )
            .map_err(|e| StryiNetworkError::other(format!("read payload: {e}")))?;

        match signing_key.clone().try_into_ed25519() {
            Ok(ref pk) if pk == expected_pk => {}
            _ => return Err(StryiNetworkError::other("signing key mismatch")),
        }

        let (rec, _len): (ServiceRecord, _) =
            bincode::serde::decode_from_slice(payload, standard())
                .map_err(|e| StryiNetworkError::other(format!("bincode : {e}")))?;

        let expected_peer = PeerId::from_public_key(&expected_pk.clone().into());
        if rec.owner != expected_peer {
            return Err(StryiNetworkError::other("owner PeerId mismatch"));
        }

        Ok(rec)
    }
}

/// Verify every `SignedServiceRecord` with `peer_pk` and return the
/// `ServiceRecord`s that passed. Invalid items are logged and skipped.
pub(crate) fn filter_verified_records(
    signed: Vec<SignedServiceRecord>,
    peer_pk: &PublicKey,
) -> Vec<ServiceRecord> {
    signed
        .into_iter()
        .filter_map(|ssr| match ssr.verify_and_decode(peer_pk) {
            Ok(rec) => Some(rec),
            Err(e) => {
                tracing::warn!("invalid ServiceRecord from peer: {e:?}");
                None
            }
        })
        .collect()
}

// Serialize -> just dump protobuf-encoded bytes
impl Serialize for SignedServiceRecord {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_bytes(self.clone().inner.into_protobuf_encoding().as_slice())
    }
}

impl<'de> Deserialize<'de> for SignedServiceRecord {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct EnvVisitor;

        impl<'de> Visitor<'de> for EnvVisitor {
            type Value = SignedServiceRecord;

            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                write!(f, "protobuf-encoded SignedEnvelope bytes")
            }

            fn visit_bytes<E>(self, v: &[u8]) -> Result<Self::Value, E>
            where
                E: SerdeError,
            {
                SignedEnvelope::from_protobuf_encoding(v)
                    .map(|env| SignedServiceRecord { inner: env })
                    .map_err(|e| E::custom(format!("SignedEnvelope decode: {e}")))
            }

            fn visit_byte_buf<E>(self, v: Vec<u8>) -> Result<Self::Value, E>
            where
                E: SerdeError,
            {
                self.visit_bytes(&v)
            }
        }

        deserializer.deserialize_bytes(EnvVisitor)
    }
}

impl From<SignedServiceRecord> for Vec<u8> {
    fn from(ssr: SignedServiceRecord) -> Self {
        ssr.inner.into_protobuf_encoding()
    }
}

impl From<&SignedServiceRecord> for Vec<u8> {
    fn from(ssr: &SignedServiceRecord) -> Self {
        ssr.clone().inner.into_protobuf_encoding().to_vec()
    }
}

impl TryFrom<&[u8]> for SignedServiceRecord {
    type Error = StryiNetworkError;

    fn try_from(src: &[u8]) -> Result<Self, Self::Error> {
        SignedEnvelope::from_protobuf_encoding(src)
            .map(|env| SignedServiceRecord { inner: env })
            .map_err(|e| StryiNetworkError::other(format!("proto decode: {e}")))
    }
}

/// Our ServiceInfo NetworkBehaviour relies on https://docs.rs/libp2p/latest/libp2p/request_response/cbor/type.Behaviour.html to perform serialization in binary format
pub type ServicesInfoBehaviour = RequestResponseBehaviour<ServicesInfoRequest, ServicesResponse>;

/*
/// Definition of an inbound request or response for service-protocol
pub(crate) type ServicesInfoMessage = Message<ServicesInfoRequest, ServicesResponse>;
*/

/// Type alias for ServicesInfo protocol events
pub type ServicesEvent = ReqRespEvent<ServicesInfoRequest, ServicesResponse>;

impl From<ServicesEvent> for StryiEvent {
    fn from(e: ServicesEvent) -> Self {
        StryiEvent::Services(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use libp2p::identity::ed25519;

    // Helper: build a ServiceRecord bound to a specific owner PeerId
    fn sample_record_for_owner(owner: PeerId) -> ServiceRecord {
        ServiceRecord::new(
            "/ip4/0.0.0.0/tcp/6001".parse().unwrap(),
            owner,
            "grpc-sync".to_string(),
            1,
            ServiceTransportSecurity::TlsServerCert {
                cert_pem: "-----BEGIN CERTIFICATE-----\nmock\n-----END CERTIFICATE-----".into(),
            },
        )
    }

    #[test]
    fn service_record_supports_plaintext_and_tls_variants() {
        let owner = PeerId::random();

        let plaintext = ServiceRecord::new(
            "/ip4/127.0.0.1/tcp/7001".parse().unwrap(),
            owner,
            "http".to_string(),
            1,
            ServiceTransportSecurity::None,
        );
        assert_eq!(
            plaintext.transport_security(),
            &ServiceTransportSecurity::None
        );

        let tls = sample_record_for_owner(owner);
        assert!(matches!(
            tls.transport_security(),
            ServiceTransportSecurity::TlsServerCert { cert_pem } if cert_pem.contains("BEGIN CERTIFICATE")
        ));
    }

    #[test]
    fn signed_service_record_serialise_roundtrip() {
        let kp = ed25519::Keypair::generate();
        let owner = PeerId::from_public_key(&kp.public().into());
        let rec = sample_record_for_owner(owner);
        let signed = SignedServiceRecord::sign(kp, rec).expect("sign");

        // bincode round-trip via bincode::serde
        let vec = bincode::serde::encode_to_vec(&signed, standard()).unwrap();
        let de: SignedServiceRecord = bincode::serde::decode_from_slice(&vec, standard())
            .unwrap()
            .0;

        // protobuf bytes must match
        assert_eq!(Vec::<u8>::from(signed.clone()), Vec::<u8>::from(de));
    }

    #[test]
    fn signed_service_full_cycle_sign_serialize_verify_decode() {
        let kp_a = ed25519::Keypair::generate();
        let owner_a = PeerId::from_public_key(&kp_a.public().into());
        let rec_a = sample_record_for_owner(owner_a);
        let signed_a = SignedServiceRecord::sign(kp_a.clone(), rec_a.clone()).expect("sign");

        let wire: Vec<u8> = signed_a.clone().into();

        let ssr_b = SignedServiceRecord::try_from(wire.as_slice()).expect("proto decode");

        let pubkey_a = kp_a.public();
        let decoded = ssr_b.verify_and_decode(&pubkey_a).unwrap();
        assert_eq!(decoded, rec_a);
    }

    #[test]
    fn filter_verifies_and_discards_bad_records() {
        let kp = ed25519::Keypair::generate();
        let pk = kp.public();

        let good_owner = PeerId::from_public_key(&pk.clone().into());
        let good_rec = sample_record_for_owner(good_owner);
        let good_signed = SignedServiceRecord::sign(kp.clone(), good_rec.clone()).unwrap();

        let other_kp = ed25519::Keypair::generate();
        let bad_owner = PeerId::from_public_key(&other_kp.public().into());
        let bad_rec = sample_record_for_owner(bad_owner);
        let bad_signed = SignedServiceRecord::sign(other_kp, bad_rec).unwrap();

        let vec = vec![good_signed, bad_signed];
        let out = filter_verified_records(vec, &pk);

        assert_eq!(out, vec![good_rec]);
    }
}
