//! Test-only proxy and TLS fixtures for the update transport and the curl
//! runtime downloader. Everything is local: a throw-away CA and leaf
//! certificate signed with `ring`, a loopback "origin" HTTPS server and a
//! loopback HTTP proxy. Nothing here reaches a real service or uses a real
//! secret, and none of it is compiled into the application.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ring::rand::SystemRandom;
use ring::signature::{ECDSA_P256_SHA256_ASN1_SIGNING, EcdsaKeyPair, KeyPair};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::{RootCertStore, ServerConfig, ServerConnection, StreamOwned};

use crate::https_transport::base64_encode;

fn der_len(length: usize) -> Vec<u8> {
    if length < 128 {
        vec![length as u8]
    } else if length < 256 {
        vec![0x81, length as u8]
    } else {
        vec![0x82, (length >> 8) as u8, length as u8]
    }
}

fn tlv(tag: u8, body: &[u8]) -> Vec<u8> {
    let mut out = vec![tag];
    out.extend(der_len(body.len()));
    out.extend_from_slice(body);
    out
}

fn seq(parts: &[Vec<u8>]) -> Vec<u8> {
    tlv(0x30, &parts.concat())
}

const OID_ECDSA_SHA256: &[u8] = &[0x06, 0x08, 0x2A, 0x86, 0x48, 0xCE, 0x3D, 0x04, 0x03, 0x02];
const OID_EC_PUBLIC_KEY: &[u8] = &[0x06, 0x07, 0x2A, 0x86, 0x48, 0xCE, 0x3D, 0x02, 0x01];
const OID_PRIME256V1: &[u8] = &[0x06, 0x08, 0x2A, 0x86, 0x48, 0xCE, 0x3D, 0x03, 0x01, 0x07];
const OID_COMMON_NAME: &[u8] = &[0x06, 0x03, 0x55, 0x04, 0x03];
const OID_BASIC_CONSTRAINTS: &[u8] = &[0x06, 0x03, 0x55, 0x1D, 0x13];
const OID_KEY_USAGE: &[u8] = &[0x06, 0x03, 0x55, 0x1D, 0x0F];
const OID_SAN: &[u8] = &[0x06, 0x03, 0x55, 0x1D, 0x11];
const OID_EKU: &[u8] = &[0x06, 0x03, 0x55, 0x1D, 0x25];
const OID_SERVER_AUTH: &[u8] = &[0x06, 0x08, 0x2B, 0x06, 0x01, 0x05, 0x05, 0x07, 0x03, 0x01];

fn name(common_name: &str) -> Vec<u8> {
    let attribute = seq(&[OID_COMMON_NAME.to_vec(), tlv(0x0C, common_name.as_bytes())]);
    seq(&[tlv(0x31, &attribute)])
}

fn extension(oid: &[u8], critical: bool, value: &[u8]) -> Vec<u8> {
    let mut parts = vec![oid.to_vec()];
    if critical {
        parts.push(vec![0x01, 0x01, 0xFF]);
    }
    parts.push(tlv(0x04, value));
    seq(&parts)
}

struct Signer {
    pair: EcdsaKeyPair,
    pkcs8: Vec<u8>,
}

fn new_signer(rng: &SystemRandom) -> Signer {
    let document = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, rng).unwrap();
    let pkcs8 = document.as_ref().to_vec();
    let pair = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &pkcs8, rng).unwrap();
    Signer { pair, pkcs8 }
}

fn spki(signer: &Signer) -> Vec<u8> {
    let mut bits = vec![0x00];
    bits.extend_from_slice(signer.pair.public_key().as_ref());
    seq(&[
        seq(&[OID_EC_PUBLIC_KEY.to_vec(), OID_PRIME256V1.to_vec()]),
        tlv(0x03, &bits),
    ])
}

#[allow(clippy::too_many_arguments)]
fn certificate(
    rng: &SystemRandom,
    serial: u8,
    issuer: &str,
    subject: &str,
    subject_key: &Signer,
    issuer_key: &Signer,
    extensions: &[Vec<u8>],
) -> Vec<u8> {
    let signature_algorithm = seq(&[OID_ECDSA_SHA256.to_vec()]);
    let validity = seq(&[
        tlv(0x17, b"200101000000Z"),
        tlv(0x17, b"491231235959Z"),
    ]);
    let tbs = seq(&[
        tlv(0xA0, &[0x02, 0x01, 0x02]),
        tlv(0x02, &[serial]),
        signature_algorithm.clone(),
        name(issuer),
        validity,
        name(subject),
        spki(subject_key),
        tlv(0xA3, &seq(extensions)),
    ]);
    let signature = issuer_key.pair.sign(rng, &tbs).unwrap();
    let mut bits = vec![0x00];
    bits.extend_from_slice(signature.as_ref());
    seq(&[tbs, signature_algorithm, tlv(0x03, &bits)])
}

/// A throw-away CA plus one server leaf certificate valid for `names`.
pub(crate) struct TestPki {
    pub ca_der: Vec<u8>,
    leaf_der: Vec<u8>,
    leaf_key: Vec<u8>,
}

impl TestPki {
    pub fn new(names: &[&str]) -> Self {
        let rng = SystemRandom::new();
        let ca_key = new_signer(&rng);
        let leaf_key = new_signer(&rng);
        let ca_der = certificate(
            &rng,
            1,
            "Hane Test CA",
            "Hane Test CA",
            &ca_key,
            &ca_key,
            &[
                extension(OID_BASIC_CONSTRAINTS, true, &seq(&[vec![0x01, 0x01, 0xFF]])),
                extension(OID_KEY_USAGE, true, &[0x03, 0x02, 0x02, 0x04]),
            ],
        );
        let alt_names: Vec<Vec<u8>> = names
            .iter()
            .map(|dns| tlv(0x82, dns.as_bytes()))
            .collect();
        let leaf_der = certificate(
            &rng,
            2,
            "Hane Test CA",
            names[0],
            &leaf_key,
            &ca_key,
            &[
                extension(OID_BASIC_CONSTRAINTS, false, &seq(&[])),
                extension(OID_KEY_USAGE, true, &[0x03, 0x02, 0x07, 0x80]),
                extension(OID_EKU, false, &seq(&[OID_SERVER_AUTH.to_vec()])),
                extension(OID_SAN, false, &seq(&alt_names)),
            ],
        );
        Self {
            ca_der,
            leaf_der,
            leaf_key: leaf_key.pkcs8,
        }
    }

    pub fn ca_pem(&self) -> String {
        let encoded = base64_encode(&self.ca_der);
        let mut pem = String::from("-----BEGIN CERTIFICATE-----\n");
        for chunk in encoded.as_bytes().chunks(64) {
            pem.push_str(std::str::from_utf8(chunk).unwrap());
            pem.push('\n');
        }
        pem.push_str("-----END CERTIFICATE-----\n");
        pem
    }

    pub fn roots(&self) -> RootCertStore {
        let mut roots = RootCertStore::empty();
        roots.add(CertificateDer::from(self.ca_der.clone())).unwrap();
        roots
    }

    fn server_config(&self) -> Arc<ServerConfig> {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let config = ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(
                vec![CertificateDer::from(self.leaf_der.clone())],
                PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(self.leaf_key.clone())),
            )
            .unwrap();
        Arc::new(config)
    }
}

/// Reads up to the end of an HTTP head (`CRLF CRLF`), one byte at a time so no
/// following byte is consumed.
fn read_head<S: Read>(stream: &mut S) -> Option<String> {
    let mut raw = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        match stream.read(&mut byte) {
            Ok(1) => raw.push(byte[0]),
            _ => return None,
        }
        if raw.ends_with(b"\r\n\r\n") {
            return String::from_utf8(raw).ok();
        }
        if raw.len() > 64 * 1024 {
            return None;
        }
    }
}

type Requests = Arc<Mutex<Vec<String>>>;

pub(crate) fn recorded(requests: &Requests) -> Vec<String> {
    requests.lock().unwrap().clone()
}

/// A loopback HTTPS origin. `handler` maps each request head to the complete
/// response bytes.
pub(crate) struct OriginFixture {
    pub addr: SocketAddr,
    pub requests: Requests,
}

pub(crate) fn spawn_origin(
    pki: &TestPki,
    handler: impl Fn(&str) -> Vec<u8> + Send + Sync + 'static,
) -> OriginFixture {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let requests: Requests = Arc::default();
    let config = pki.server_config();
    let handler = Arc::new(handler);
    let log = Arc::clone(&requests);
    std::thread::spawn(move || {
        for tcp in listener.incoming() {
            let Ok(tcp) = tcp else { return };
            let config = Arc::clone(&config);
            let handler = Arc::clone(&handler);
            let log = Arc::clone(&log);
            std::thread::spawn(move || {
                let _ = tcp.set_read_timeout(Some(Duration::from_secs(10)));
                let Ok(connection) = ServerConnection::new(config) else {
                    return;
                };
                let mut tls = StreamOwned::new(connection, tcp);
                // A client that rejects the certificate ends the handshake
                // here, before any request is recorded.
                let Some(head) = read_head(&mut tls) else {
                    return;
                };
                log.lock().unwrap().push(head.clone());
                let _ = tls.write_all(&handler(&head));
                let _ = tls.flush();
                tls.conn.send_close_notify();
                let _ = tls.flush();
            });
        }
    });
    OriginFixture { addr, requests }
}

/// A complete `200` response with a body.
pub(crate) fn ok_response(body: &[u8]) -> Vec<u8> {
    let mut out = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .into_bytes();
    out.extend_from_slice(body);
    out
}

pub(crate) enum ProxyBehavior {
    /// Open a tunnel to `origin`. With `require_basic` set, a request without
    /// exactly that `Proxy-Authorization` value gets a `407`.
    Tunnel {
        origin: SocketAddr,
        require_basic: Option<String>,
    },
    /// Write these chunks (a short pause between them), then close.
    Raw(Vec<Vec<u8>>),
    /// Accept and say nothing until the client goes away.
    Silent,
}

pub(crate) struct ProxyFixture {
    pub addr: SocketAddr,
    /// Every CONNECT head the proxy received.
    pub requests: Requests,
}

pub(crate) fn spawn_proxy(behavior: ProxyBehavior) -> ProxyFixture {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let requests: Requests = Arc::default();
    let log = Arc::clone(&requests);
    let behavior = Arc::new(behavior);
    std::thread::spawn(move || {
        for client in listener.incoming() {
            let Ok(client) = client else { return };
            let behavior = Arc::clone(&behavior);
            let log = Arc::clone(&log);
            std::thread::spawn(move || serve_proxy_client(client, &behavior, &log));
        }
    });
    ProxyFixture { addr, requests }
}

fn serve_proxy_client(mut client: TcpStream, behavior: &ProxyBehavior, log: &Requests) {
    let _ = client.set_read_timeout(Some(Duration::from_secs(10)));
    let Some(head) = read_head(&mut client) else {
        return;
    };
    log.lock().unwrap().push(head.clone());
    match behavior {
        ProxyBehavior::Silent => {
            let mut sink = [0u8; 64];
            while matches!(client.read(&mut sink), Ok(n) if n > 0) {}
        }
        ProxyBehavior::Raw(chunks) => {
            for chunk in chunks {
                if client.write_all(chunk).is_err() {
                    return;
                }
                let _ = client.flush();
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        ProxyBehavior::Tunnel {
            origin,
            require_basic,
        } => {
            if let Some(expected) = require_basic {
                let wanted = format!("Proxy-Authorization: Basic {expected}\r\n");
                if !head.contains(&wanted) {
                    let _ = client.write_all(
                        b"HTTP/1.1 407 Proxy Authentication Required\r\nProxy-Authenticate: Basic realm=\"test\"\r\nContent-Length: 0\r\n\r\n",
                    );
                    return;
                }
            }
            let Ok(upstream) = TcpStream::connect(origin) else {
                let _ = client.write_all(b"HTTP/1.1 502 Bad Gateway\r\n\r\n");
                return;
            };
            if client
                .write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
                .is_err()
            {
                return;
            }
            let (Ok(mut client_read), Ok(mut upstream_write)) =
                (client.try_clone(), upstream.try_clone())
            else {
                return;
            };
            let mut upstream_read = upstream;
            let forward = std::thread::spawn(move || {
                let _ = std::io::copy(&mut client_read, &mut upstream_write);
                let _ = upstream_write.shutdown(std::net::Shutdown::Write);
            });
            let _ = std::io::copy(&mut upstream_read, &mut client);
            let _ = client.shutdown(std::net::Shutdown::Write);
            let _ = forward.join();
        }
    }
}
