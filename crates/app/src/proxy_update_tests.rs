//! Update flow through the production `HttpsTransport` and `Fetcher` behind a
//! loopback HTTP proxy and a loopback TLS origin (Issue #623, P3 / P5).
//!
//! The flow is the real one: `releases/latest` → signed catalog and signature →
//! trust verification → archive download into a part store, where the archive
//! size and SHA-256 come from the signed catalog. The origin hosts are the
//! GitHub names the production allowlists accept; only the trust root and the
//! name table (which resolves nothing but the proxy, so a direct connection
//! cannot succeed) are test seams. No production trust key or allowlist is
//! loosened, and nothing reaches a real service.

use hane_network::{EnvPlatform, NetworkContext, ProxyEnvironment};
use hane_update::catalog::{VerifyContext, confirm_selection, verify_catalog};
use hane_update::download::{
    ArchiveDownload, CancelToken, DownloadedArchive, Fetcher, asset_url, latest_release_url,
};
use hane_update::error::{ErrorCode, Result};
use hane_update::model::{CATALOG_ASSET_NAME, CATALOG_DOMAIN, SIGNATURE_ASSET_NAME, Target};
use hane_update::testing::{
    FixedClock, MemoryPartStore, TEST_KEY_ID, WindowsRelease, default_payload, release_json_sized,
    sign_with, test_now, test_policy, windows_release,
};
use hane_update::version::parse_current_version;

use crate::https_transport::{HttpsTransport, LookupLog, base64_encode};
use crate::test_tls::{
    OriginFixture, ProxyBehavior, ProxyFixture, TestPki, ok_response, recorded, spawn_origin,
    spawn_proxy,
};

const TAG: &str = "v0.27.0";
const CURRENT: &str = "0.26.0";
const OBJECTS_URL: &str = "https://objects.githubusercontent.com/blob/1?sig=abc";
const PROXY_SECRET: &[u8] = b"user:pa:ss";
const HOSTS: [&str; 4] = [
    "api.github.com",
    "github.com",
    "objects.githubusercontent.com",
    "release-assets.githubusercontent.com",
];

/// What the fake GitHub origin serves.
struct Served {
    catalog: Vec<u8>,
    signature: Vec<u8>,
    zip: Vec<u8>,
    /// `Location` of the archive asset on `github.com`.
    zip_location: String,
    /// Cancelled by the origin right before it answers the archive request.
    cancel_on_zip: Option<CancelToken>,
    /// The `Proxy-Authorization` Basic value the proxy insists on.
    proxy_expects: String,
}

impl Served {
    fn good(release: &WindowsRelease) -> Self {
        Self {
            catalog: release.catalog.clone(),
            signature: release.catalog_sig.clone(),
            zip: release.zip.clone(),
            zip_location: OBJECTS_URL.to_owned(),
            cancel_on_zip: None,
            proxy_expects: base64_encode(PROXY_SECRET),
        }
    }
}

struct Fixture {
    proxy: ProxyFixture,
    origin: OriginFixture,
    transport: HttpsTransport,
    lookups: LookupLog,
}

fn response(status_line: &str, extra_headers: &str, body: &[u8]) -> Vec<u8> {
    let mut out = format!(
        "HTTP/1.1 {status_line}\r\n{extra_headers}Content-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .into_bytes();
    out.extend_from_slice(body);
    out
}

/// `(host, path)` of a request head.
fn target_of(head: &str) -> (String, String) {
    let path = head
        .lines()
        .next()
        .and_then(|line| line.split(' ').nth(1))
        .unwrap_or("")
        .to_owned();
    let host = head
        .lines()
        .find_map(|line| line.strip_prefix("Host: "))
        .unwrap_or("")
        .to_owned();
    (host, path)
}

fn start(release: &WindowsRelease, served: Served) -> Fixture {
    let pki = TestPki::new(&HOSTS);
    let listing = release_json_sized(
        TAG,
        &[
            (CATALOG_ASSET_NAME, release.catalog.len() as u64),
            (SIGNATURE_ASSET_NAME, release.catalog_sig.len() as u64),
            (Target::WindowsX64.asset_name(), release.zip.len() as u64),
        ],
        false,
        false,
    );
    let archive_url = asset_url(TAG, Target::WindowsX64.asset_name());
    let catalog_url = asset_url(TAG, CATALOG_ASSET_NAME);
    let signature_url = asset_url(TAG, SIGNATURE_ASSET_NAME);
    let latest = latest_release_url();
    let Served {
        catalog,
        signature,
        zip,
        zip_location,
        cancel_on_zip,
        proxy_expects,
    } = served;

    let origin = spawn_origin(&pki, move |head| {
        let (host, path) = target_of(head);
        let url = format!("https://{host}{path}");
        if url == latest {
            ok_response(&listing)
        } else if url == catalog_url {
            ok_response(&catalog)
        } else if url == signature_url {
            ok_response(&signature)
        } else if url == archive_url {
            if let Some(cancel) = &cancel_on_zip {
                cancel.cancel();
            }
            response("302 Found", &format!("Location: {zip_location}\r\n"), &[])
        } else if url == OBJECTS_URL {
            ok_response(&zip)
        } else {
            response("404 Not Found", "", &[])
        }
    });
    let proxy = spawn_proxy(ProxyBehavior::Tunnel {
        origin: origin.addr,
        require_basic: Some(proxy_expects),
    });
    let url = format!("http://user:pa%3Ass@proxy.test:{}", proxy.addr.port());
    let network = NetworkContext::from_environment(ProxyEnvironment::from_vars(
        EnvPlatform::MacOs,
        [("https_proxy", url.as_str())],
    ));
    // Only the proxy resolves. The origin names are not in the table, so a
    // direct connection to them fails before any socket is opened.
    let (transport, lookups) = HttpsTransport::with_test_seams(
        pki.roots(),
        network,
        vec![("proxy.test", proxy.addr)],
        443,
    );
    Fixture {
        proxy,
        origin,
        transport,
        lookups,
    }
}

/// API → catalog / signature → verification → size / hash checked staging.
fn run_update(
    fixture: &Fixture,
    store: &mut MemoryPartStore,
    cancel: &CancelToken,
) -> Result<DownloadedArchive> {
    let clock = FixedClock::new(test_now());
    let fetcher = Fetcher {
        transport: &fixture.transport,
        clock: &clock,
    };
    let current = parse_current_version(CURRENT)?;
    let selection = fetcher.check_latest(&current, Target::WindowsX64, cancel)?;
    let (catalog, signature) = fetcher.fetch_catalog(&selection, cancel)?;
    let policy = test_policy();
    let candidate = verify_catalog(
        &catalog,
        &signature,
        &VerifyContext {
            policy: &policy,
            clock: &clock,
            current_version: CURRENT,
            target: Target::WindowsX64,
            seen: None,
            expected_tag: Some(&selection.tag),
        },
    )?;
    confirm_selection(&selection, &candidate)?;
    let entry = candidate.target_entry();
    fetcher.download_archive(
        &ArchiveDownload {
            tag: &selection.tag,
            name: &entry.asset_name,
            size: entry.archive_size,
            sha256: &entry.archive_sha256,
        },
        store,
        cancel,
    )
}

fn release() -> WindowsRelease {
    windows_release("0.27.0", 1, default_payload("0.27.0"))
}

/// `host:port` of every CONNECT the proxy received, in order.
fn connect_targets(fixture: &Fixture) -> Vec<String> {
    recorded(&fixture.proxy.requests)
        .iter()
        .map(|head| {
            let line = head.lines().next().unwrap_or("");
            assert!(line.starts_with("CONNECT "), "{line}");
            line.split(' ').nth(1).unwrap_or("").to_owned()
        })
        .collect()
}

/// Every lookup was for the proxy: nothing resolved or dialed an origin name.
fn assert_only_the_proxy_was_resolved(fixture: &Fixture) {
    let lookups = fixture.lookups.lock().unwrap().clone();
    assert!(!lookups.is_empty());
    for (host, port) in lookups {
        assert_eq!(host, "proxy.test");
        assert_eq!(port, fixture.proxy.addr.port());
    }
}

fn code<T>(result: Result<T>) -> ErrorCode {
    result.err().expect("expected an error").code
}

#[test]
fn a_signed_release_is_staged_through_the_proxy_without_a_direct_connection() {
    let release = release();
    let fixture = start(&release, Served::good(&release));
    let mut store = MemoryPartStore::default();

    let done = run_update(&fixture, &mut store, &CancelToken::new())
        .unwrap_or_else(|error| panic!("{error:?}"));
    assert_eq!(done.size, release.zip.len() as u64);
    assert_eq!(store.committed().unwrap(), release.zip);

    // Every hop, including the allowed redirect to the delivery host, is its
    // own CONNECT with the proxy credential.
    assert_eq!(
        connect_targets(&fixture),
        vec![
            "api.github.com:443",
            "github.com:443",
            "github.com:443",
            "github.com:443",
            "objects.githubusercontent.com:443",
        ]
    );
    let credential = format!("Proxy-Authorization: Basic {}\r\n", base64_encode(PROXY_SECRET));
    for head in recorded(&fixture.proxy.requests) {
        assert!(head.contains(&credential), "{head}");
    }

    // The origin saw each request, and never the proxy credential.
    let origin_requests = recorded(&fixture.origin.requests);
    assert_eq!(origin_requests.len(), 5, "{origin_requests:?}");
    assert!(origin_requests[0].starts_with("GET /repos/"), "{}", origin_requests[0]);
    assert!(origin_requests[4].starts_with("GET /blob/1?sig=abc HTTP/1.1\r\n"));
    let encoded = base64_encode(PROXY_SECRET);
    for head in &origin_requests {
        assert!(!head.to_ascii_lowercase().contains("proxy-authorization"), "{head}");
        assert!(!head.contains(&encoded), "{head}");
    }

    // The origin names are not resolvable, so success proves the tunnel.
    assert_only_the_proxy_was_resolved(&fixture);
    assert_eq!(fixture.lookups.lock().unwrap().len(), 5);
}

#[test]
fn a_proxy_authentication_failure_ends_the_check_before_any_origin_request() {
    let release = release();
    let mut served = Served::good(&release);
    served.proxy_expects = base64_encode(b"other:secret");
    let fixture = start(&release, served);
    let mut store = MemoryPartStore::default();

    assert_eq!(
        code(run_update(&fixture, &mut store, &CancelToken::new())),
        ErrorCode::ProxyAuthentication
    );
    assert!(recorded(&fixture.origin.requests).is_empty());
    assert_only_the_proxy_was_resolved(&fixture);
    assert!(store.committed().is_none());
}

#[test]
fn a_redirect_outside_the_release_hosts_never_reaches_the_proxy() {
    let release = release();
    for location in [
        "https://evil.example.com/x.zip",
        "http://objects.githubusercontent.com/x.zip",
        "https://objects.githubusercontent.com:8443/x.zip",
        "https://objects.githubusercontent.com.evil.example/x.zip",
        "https://user:pw@objects.githubusercontent.com/x.zip",
    ] {
        let mut served = Served::good(&release);
        served.zip_location = location.to_owned();
        let fixture = start(&release, served);
        let mut store = MemoryPartStore::default();

        assert_eq!(
            code(run_update(&fixture, &mut store, &CancelToken::new())),
            ErrorCode::RedirectRejected,
            "{location}"
        );
        // Only the four allowed requests were tunnelled; the rejected target
        // was never a CONNECT, never resolved.
        assert_eq!(
            connect_targets(&fixture),
            vec![
                "api.github.com:443",
                "github.com:443",
                "github.com:443",
                "github.com:443",
            ],
            "{location}"
        );
        assert_only_the_proxy_was_resolved(&fixture);
        assert!(store.committed().is_none(), "{location}");
        assert!(store.was_discarded(), "{location}");
    }
}

#[test]
fn a_bad_signature_stops_the_flow_before_the_archive_is_requested() {
    let release = release();
    let forged_signature = sign_with(2, TEST_KEY_ID, CATALOG_DOMAIN, &release.catalog);
    let mut tampered_catalog = release.catalog.clone();
    tampered_catalog.push(b' ');

    let cases = [
        ("wrong key", release.catalog.clone(), forged_signature),
        ("tampered bytes", tampered_catalog, release.catalog_sig.clone()),
    ];
    for (label, catalog, signature) in cases {
        let mut served = Served::good(&release);
        served.catalog = catalog;
        served.signature = signature;
        let fixture = start(&release, served);
        let mut store = MemoryPartStore::default();

        let result = run_update(&fixture, &mut store, &CancelToken::new());
        assert!(result.is_err(), "{label}: unsigned catalog was accepted");
        assert_eq!(
            connect_targets(&fixture),
            vec!["api.github.com:443", "github.com:443", "github.com:443"],
            "{label}"
        );
        assert!(store.committed().is_none(), "{label}");
    }
}

#[test]
fn an_archive_that_differs_from_the_signed_catalog_is_discarded() {
    let release = release();

    let mut flipped = release.zip.clone();
    flipped[0] ^= 0xFF;
    let mut shorter = release.zip.clone();
    shorter.pop();
    let mut longer = release.zip.clone();
    longer.push(0);

    let cases = [
        ("hash", flipped, ErrorCode::HashMismatch),
        ("short", shorter, ErrorCode::SizeMismatch),
        ("long", longer, ErrorCode::LimitExceeded),
    ];
    for (label, zip, expected) in cases {
        let mut served = Served::good(&release);
        served.zip = zip;
        let fixture = start(&release, served);
        let mut store = MemoryPartStore::default();

        assert_eq!(
            code(run_update(&fixture, &mut store, &CancelToken::new())),
            expected,
            "{label}"
        );
        assert!(store.committed().is_none(), "{label}");
        assert!(store.was_discarded(), "{label}: the part file must be removed");
        assert_only_the_proxy_was_resolved(&fixture);
    }
}

#[test]
fn a_cancel_during_the_proxied_transfer_leaves_no_part_file() {
    let release = release();
    let cancel = CancelToken::new();
    let mut served = Served::good(&release);
    served.cancel_on_zip = Some(cancel.clone());
    let fixture = start(&release, served);
    let mut store = MemoryPartStore::default();

    assert_eq!(
        code(run_update(&fixture, &mut store, &cancel)),
        ErrorCode::Cancelled
    );
    assert!(store.committed().is_none());
    assert!(store.was_discarded());
    assert_only_the_proxy_was_resolved(&fixture);
}
