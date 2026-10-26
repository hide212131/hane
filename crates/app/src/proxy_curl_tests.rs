//! Real-`curl` tests of the runtime archive downloader behind a loopback HTTP
//! proxy and a loopback TLS origin (Issue #623, P6). They drive the production
//! `CurlArchiveDownloader` (route decision, argv, stdin config, environment
//! handling); only the CA bundle and the fixture address mapping are added
//! through the test seam. A missing `curl` fails these tests instead of
//! skipping them, because a skipped run is not evidence.

use std::ffi::OsString;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use hane_ai::{
    ArchiveDownloader, CurlArchiveDownloader, DownloadFailure, RuntimeInstallErrorCode,
    RuntimeInstallSpec,
};
use hane_network::{
    BasicCredentials, CredentialError, CredentialRef, CredentialStore, EnvPlatform, ManualProxy,
    NetworkContext, NetworkSettings, ProxyAuth, ProxyEnvironment,
};

use crate::https_transport::base64_encode;
use crate::test_tls::{ProxyBehavior, TestPki, ok_response, recorded, spawn_origin, spawn_proxy};

fn require_curl() {
    let available = Command::new("curl")
        .arg("--version")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false);
    assert!(
        available,
        "curl is required for the proxy download tests; a missing curl is a failure, not a skip"
    );
}

fn scratch(label: &str) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "hane-curl-proxy-{label}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn spec(url: &str, size: u64) -> RuntimeInstallSpec {
    RuntimeInstallSpec {
        target: "test".to_owned(),
        codex_version: "0".to_owned(),
        source_commit: "test".to_owned(),
        url: url.to_owned(),
        archive_size: size,
        archive_sha256: "0".repeat(64),
        entrypoint_name: "x".to_owned(),
        allowed_hosts: vec![
            "github.com".to_owned(),
            "objects.githubusercontent.com".to_owned(),
        ],
        pinned_inventory: Vec::new(),
        runtime_profile: "default".to_owned(),
        config_profile: "default".to_owned(),
        state_profile: "default".to_owned(),
    }
}

fn env_network(vars: &[(&str, &str)]) -> NetworkContext {
    NetworkContext::from_environment(ProxyEnvironment::from_vars(
        EnvPlatform::current(),
        vars.iter().copied(),
    ))
}

/// A downloader that trusts only `pki` and reaches `connect_to` hosts on the
/// fixture origin. Hosts that go through a proxy must not be listed.
fn downloader(
    network: NetworkContext,
    pki: &TestPki,
    dir: &Path,
    connect_to: &[(&str, SocketAddr)],
    envs: Vec<(OsString, OsString)>,
) -> CurlArchiveDownloader {
    let ca = dir.join("ca.pem");
    std::fs::write(&ca, pki.ca_pem()).unwrap();
    let mut args: Vec<OsString> = vec![
        "--cacert".into(),
        ca.into_os_string(),
        // The fixture certificate has no revocation endpoint.
        "--ssl-revoke-best-effort".into(),
    ];
    for (host, address) in connect_to {
        args.push("--connect-to".into());
        args.push(format!("{host}:443:127.0.0.1:{}", address.port()).into());
    }
    CurlArchiveDownloader::new(network).with_test_seams(args, envs)
}

fn body_of(size: usize) -> Vec<u8> {
    (0..size).map(|index| (index % 251) as u8).collect()
}

fn not_canceled() -> bool {
    false
}

#[test]
fn a_proxy_without_credentials_carries_the_download() {
    require_curl();
    let dir = scratch("plain");
    let pki = TestPki::new(&["github.com"]);
    let body = body_of(5000);
    let served = body.clone();
    let origin = spawn_origin(&pki, move |_| ok_response(&served));
    let proxy = spawn_proxy(ProxyBehavior::Tunnel {
        origin: origin.addr,
        require_basic: None,
    });
    let url = format!("http://127.0.0.1:{}", proxy.addr.port());
    let curl = downloader(env_network(&[("https_proxy", url.as_str())]), &pki, &dir, &[], vec![]);
    let dest = dir.join("archive.tar.gz");

    let result = curl.download(
        &spec("https://github.com/o/r/package.tar.gz", body.len() as u64),
        &dest,
        &not_canceled,
    );
    assert_eq!(result, Ok(()), "{:?}", curl.last_failure());
    assert_eq!(std::fs::read(&dest).unwrap(), body);
    let connects = recorded(&proxy.requests);
    assert_eq!(connects.len(), 1, "{connects:?}");
    assert!(connects[0].starts_with("CONNECT github.com:443 "), "{connects:?}");
    assert!(!connects[0].contains("Proxy-Authorization"));
    assert_eq!(recorded(&origin.requests).len(), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn basic_credentials_with_quotes_backslashes_and_colons_reach_only_the_proxy() {
    require_curl();
    let dir = scratch("basic");
    let pki = TestPki::new(&["github.com"]);
    let body = body_of(1500);
    let served = body.clone();
    let origin = spawn_origin(&pki, move |_| ok_response(&served));
    // The password is `p:"\x` once the environment URL is decoded.
    let expected = base64_encode("user:p:\"\\x".as_bytes());
    let proxy = spawn_proxy(ProxyBehavior::Tunnel {
        origin: origin.addr,
        require_basic: Some(expected.clone()),
    });
    let url = format!("http://user:p%3A%22%5Cx@127.0.0.1:{}", proxy.addr.port());
    let curl = downloader(env_network(&[("https_proxy", url.as_str())]), &pki, &dir, &[], vec![]);
    let dest = dir.join("archive.tar.gz");

    let result = curl.download(
        &spec("https://github.com/o/r/package.tar.gz", body.len() as u64),
        &dest,
        &not_canceled,
    );
    assert_eq!(result, Ok(()), "{:?}", curl.last_failure());
    let connects = recorded(&proxy.requests);
    assert_eq!(connects.len(), 1, "{connects:?}");
    assert!(connects[0].contains(&format!("Proxy-Authorization: Basic {expected}\r\n")));
    let origin_requests = recorded(&origin.requests);
    assert_eq!(origin_requests.len(), 1);
    assert!(
        !origin_requests[0]
            .to_ascii_lowercase()
            .contains("proxy-authorization"),
        "the proxy credential must not reach the origin"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_rejected_proxy_credential_is_a_407_and_leaves_no_part() {
    require_curl();
    let dir = scratch("407");
    let pki = TestPki::new(&["github.com"]);
    let origin = spawn_origin(&pki, |_| ok_response(b"unused"));
    let proxy = spawn_proxy(ProxyBehavior::Tunnel {
        origin: origin.addr,
        require_basic: Some(base64_encode(b"someone:else")),
    });
    let url = format!("http://user:pw@127.0.0.1:{}", proxy.addr.port());
    let curl = downloader(env_network(&[("https_proxy", url.as_str())]), &pki, &dir, &[], vec![]);
    let dest = dir.join("archive.tar.gz");

    let result = curl.download(&spec("https://github.com/o/r/p.tar.gz", 6), &dest, &not_canceled);
    assert_eq!(result, Err(RuntimeInstallErrorCode::NetworkDownloadFailed));
    assert_eq!(curl.last_failure(), Some(DownloadFailure::ProxyAuthentication));
    assert!(!dest.exists());
    assert!(recorded(&origin.requests).is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_proxy_refusal_and_an_origin_status_stay_distinct() {
    require_curl();
    let dir = scratch("distinct");
    let pki = TestPki::new(&["github.com"]);

    let refusing = spawn_proxy(ProxyBehavior::Raw(vec![
        b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n".to_vec(),
    ]));
    let url = format!("http://127.0.0.1:{}", refusing.addr.port());
    let curl = downloader(env_network(&[("https_proxy", url.as_str())]), &pki, &dir, &[], vec![]);
    let dest = dir.join("archive.tar.gz");
    let result = curl.download(&spec("https://github.com/o/r/p.tar.gz", 6), &dest, &not_canceled);
    assert_eq!(result, Err(RuntimeInstallErrorCode::NetworkDownloadFailed));
    assert_eq!(curl.last_failure(), Some(DownloadFailure::ProxyConnect(403)));

    let origin = spawn_origin(&pki, |_| {
        b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec()
    });
    let curl = downloader(
        NetworkContext::direct(),
        &pki,
        &dir,
        &[("github.com", origin.addr)],
        vec![],
    );
    let result = curl.download(&spec("https://github.com/o/r/p.tar.gz", 6), &dest, &not_canceled);
    assert_eq!(result, Err(RuntimeInstallErrorCode::NetworkDownloadFailed));
    assert_eq!(curl.last_failure(), Some(DownloadFailure::OriginStatus(404)));
    assert!(!dest.exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn parent_proxy_variables_and_curlrc_do_not_change_the_chosen_route() {
    require_curl();
    let dir = scratch("parent");
    let pki = TestPki::new(&["github.com"]);
    let body = body_of(900);
    let served = body.clone();
    let origin = spawn_origin(&pki, move |_| ok_response(&served));
    // A curlrc that would send everything to a dead proxy.
    let home = dir.join("home");
    std::fs::create_dir_all(&home).unwrap();
    for name in [".curlrc", "_curlrc"] {
        std::fs::write(home.join(name), "proxy = \"http://127.0.0.1:1\"\n").unwrap();
    }
    let conflicting = |name: &str, value: &str| (OsString::from(name), OsString::from(value));
    let envs = vec![
        conflicting("HTTPS_PROXY", "http://127.0.0.1:1"),
        conflicting("https_proxy", "http://127.0.0.1:1"),
        conflicting("ALL_PROXY", "http://127.0.0.1:1"),
        conflicting("all_proxy", "http://127.0.0.1:1"),
        (OsString::from("CURL_HOME"), home.clone().into_os_string()),
        (OsString::from("HOME"), home.into_os_string()),
    ];
    let curl = downloader(
        NetworkContext::direct(),
        &pki,
        &dir,
        &[("github.com", origin.addr)],
        envs,
    );
    let dest = dir.join("archive.tar.gz");
    let result = curl.download(
        &spec("https://github.com/o/r/package.tar.gz", body.len() as u64),
        &dest,
        &not_canceled,
    );
    assert_eq!(result, Ok(()), "{:?}", curl.last_failure());
    assert_eq!(std::fs::read(&dest).unwrap(), body);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn every_redirect_hop_is_routed_again() {
    require_curl();
    let dir = scratch("redirect");
    let pki = TestPki::new(&["github.com", "objects.githubusercontent.com"]);
    let body = body_of(2048);
    let served = body.clone();
    let origin = spawn_origin(&pki, move |head| {
        if head.starts_with("GET /start ") {
            b"HTTP/1.1 302 Found\r\nLocation: https://objects.githubusercontent.com/asset\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec()
        } else {
            ok_response(&served)
        }
    });
    let proxy = spawn_proxy(ProxyBehavior::Tunnel {
        origin: origin.addr,
        require_basic: None,
    });
    let url = format!("http://127.0.0.1:{}", proxy.addr.port());
    let curl = downloader(
        env_network(&[
            ("https_proxy", url.as_str()),
            ("no_proxy", "objects.githubusercontent.com"),
        ]),
        &pki,
        &dir,
        &[("objects.githubusercontent.com", origin.addr)],
        vec![],
    );
    let dest = dir.join("archive.tar.gz");
    let result = curl.download(
        &spec("https://github.com/start", body.len() as u64),
        &dest,
        &not_canceled,
    );
    assert_eq!(result, Ok(()), "{:?}", curl.last_failure());
    assert_eq!(std::fs::read(&dest).unwrap(), body);
    let connects = recorded(&proxy.requests);
    assert_eq!(connects.len(), 1, "only the first hop uses the proxy: {connects:?}");
    assert!(connects[0].starts_with("CONNECT github.com:443 "));
    assert_eq!(recorded(&origin.requests).len(), 2);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cancelling_a_stalled_proxy_stops_curl_and_removes_the_part() {
    require_curl();
    let dir = scratch("cancel");
    let pki = TestPki::new(&["github.com"]);
    let proxy = spawn_proxy(ProxyBehavior::Silent);
    let url = format!("http://127.0.0.1:{}", proxy.addr.port());
    let curl = downloader(env_network(&[("https_proxy", url.as_str())]), &pki, &dir, &[], vec![]);
    let dest = dir.join("archive.tar.gz");
    std::fs::write(&dest, b"partial").unwrap();

    let started = Instant::now();
    let canceled = move || started.elapsed() > Duration::from_millis(600);
    let result = curl.download(&spec("https://github.com/o/r/p.tar.gz", 6), &dest, &canceled);
    assert_eq!(result, Err(RuntimeInstallErrorCode::Canceled));
    assert!(started.elapsed() < Duration::from_secs(20));
    assert!(!dest.exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_unusable_proxy_credential_stops_before_curl_runs() {
    struct Missing;
    impl CredentialStore for Missing {
        fn load(&self, _: &CredentialRef) -> Result<BasicCredentials, CredentialError> {
            Err(CredentialError::Unavailable)
        }
    }
    let dir = scratch("config");
    let manual = ManualProxy::new(
        "http://127.0.0.1:1",
        "",
        ProxyAuth::Basic(CredentialRef::new("c1").unwrap()),
    )
    .unwrap();
    let network = NetworkContext::new(
        NetworkSettings::manual(manual),
        ProxyEnvironment::from_vars(EnvPlatform::current(), std::iter::empty::<(&str, &str)>()),
        Arc::new(Missing),
    );
    // No curl is needed: the failure happens while preparing the hop.
    let curl = CurlArchiveDownloader::new(network);
    let dest = dir.join("archive.tar.gz");
    let result = curl.download(&spec("https://github.com/o/r/p.tar.gz", 6), &dest, &not_canceled);
    assert_eq!(result, Err(RuntimeInstallErrorCode::NetworkDownloadFailed));
    assert_eq!(curl.last_failure(), Some(DownloadFailure::ProxyConfiguration));
    let _ = std::fs::remove_dir_all(&dir);
}
