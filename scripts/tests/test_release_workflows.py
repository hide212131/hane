"""Static wiring checks for the update release pipeline (Issue #510, stage U2).

These do not run any workflow. They pin the structure that makes the pipeline
safe: secret scoping, Draft-before-stable ordering, serialized sequence
selection, the shared verifier and the shared requirements file. A helper or
catalog script on its own is not treated as a connected pipeline.
"""
from __future__ import annotations

import re
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]


def read(relative: str) -> str:
    return (ROOT / relative).read_text(encoding="utf-8")


RELEASE = read(".github/workflows/windows-release.yml")
REQUEST = read(".github/workflows/release-request.yml")
CI = read(".github/workflows/ci.yml")
BUNDLE = read("scripts/bundle-macos.sh")


def jobs(text: str) -> dict[str, str]:
    body = text.split("\njobs:\n", 1)[1]
    parts = re.split(r"(?m)^  ([a-z][a-z0-9_-]*):\s*$", body)
    return {parts[i]: parts[i + 1] for i in range(1, len(parts), 2)}


def steps(job: str) -> list[tuple[str, str]]:
    """(name, text) for each step of a job body."""
    chunks = re.split(r"(?m)^      - ", job.split("\n    steps:\n", 1)[1])
    result = []
    for chunk in chunks[1:]:
        match = re.search(r"name: (.+)", chunk)
        result.append((match.group(1).strip() if match else chunk.splitlines()[0], chunk))
    return result


def step_index(job: str, name_prefix: str) -> int:
    for index, (name, _) in enumerate(steps(job)):
        if name.startswith(name_prefix):
            return index
    raise AssertionError(f"no step named {name_prefix!r}")


def step_text(job: str, name_prefix: str) -> str:
    return steps(job)[step_index(job, name_prefix)][1]


class SecretScopingTests(unittest.TestCase):
    ALLOWED = {
        "HANE_UPDATE_SIGNING_KEY_BASE64": {"Generate and sign Windows inventory",
                                           "Generate and sign macOS inventory", "Sign catalog"},
        "HANE_MSIX_PFX_BASE64": {"Prepare Windows Explorer package signing certificate"},
        "HANE_MSIX_PFX_PASSWORD": {"Prepare Windows Explorer package signing certificate"},
    }

    def test_each_secret_reaches_only_its_signing_step(self):
        seen = set()
        for job_name, body in jobs(RELEASE).items():
            self.assertNotRegex(body.split("\n    steps:\n", 1)[0], r"secrets\.", f"{job_name} header")
            for name, text in steps(body):
                for secret in re.findall(r"secrets\.([A-Z0-9_]+)", text):
                    seen.add(secret)
                    if name == "Require release signing configuration":
                        continue
                    self.assertIn(name, self.ALLOWED.get(secret, set()), f"{secret} leaked into step {name!r}")
        self.assertEqual(seen, set(self.ALLOWED))

    def test_prepare_only_evaluates_presence_booleans(self):
        text = step_text(jobs(RELEASE)["prepare"], "Require release signing configuration")
        refs = re.findall(r"\$\{\{ ((?:secrets|vars)\.[A-Z0-9_]+)([^}]*)\}\}", text)
        self.assertTrue(refs)
        for ref, rest in refs:
            self.assertEqual(rest.strip(), "!= ''", ref)
        self.assertIn("exit 1", text)
        self.assertNotIn("echo \"$", text.split("run: |", 1)[0])

    def test_unconfigured_release_fails_instead_of_skipping(self):
        prepare = jobs(RELEASE)["prepare"]
        text = step_text(prepare, "Require release signing configuration")
        for name in ("HANE_UPDATE_SIGNING_KEY_BASE64", "HANE_UPDATE_KEY_ID", "HANE_UPDATE_PUBLIC_KEY_BASE64",
                     "HANE_MSIX_PFX_BASE64"):
            self.assertIn(f"HAS_{name}", text)
        self.assertNotRegex(text, r"APPLE|TEAM_ID")
        for job in ("build-windows", "build-macos", "release"):
            self.assertIn("needs.prepare.outputs.should_release == 'true'", jobs(RELEASE)[job])

    def test_no_secret_value_is_written_to_logs_or_env_files(self):
        for forbidden in ("GITHUB_ENV", "echo \"$HANE_UPDATE_SIGNING_KEY", "set -x"):
            for job_name, body in jobs(RELEASE).items():
                for name, text in steps(body):
                    if name in {s for names in self.ALLOWED.values() for s in names}:
                        if forbidden == "GITHUB_ENV" and name.startswith("Prepare Windows Explorer"):
                            continue  # exports only the certificate thumbprint
                        self.assertNotIn(forbidden, text, f"{job_name}/{name}")


class ReleaseJobTests(unittest.TestCase):
    def setUp(self):
        self.job = jobs(RELEASE)["release"]

    def test_release_job_is_serialized_across_all_entry_points(self):
        self.assertRegex(self.job, r"concurrency:\s+group: hane-release-publish\s+cancel-in-progress: false")
        self.assertNotIn("concurrency:", RELEASE.split("\njobs:\n", 1)[0])

    def test_sequence_selection_signing_draft_and_publish_order(self):
        order = [
            "Plan release",
            "Build catalog from the real ZIP files",
            "Sign catalog",
            "Verify assets before creating the Draft",
            "Create Draft release and upload assets",
            "Re-download and verify the Draft assets",
            "Publish the verified Draft as stable",
            "Verify the published Release again",
        ]
        indexes = [step_index(self.job, name) for name in order]
        self.assertEqual(indexes, sorted(indexes))
        self.assertEqual(len(set(indexes)), len(indexes))

    def test_stable_publication_happens_only_after_redownload_verification(self):
        draft = step_text(self.job, "Create Draft release and upload assets")
        verify = step_text(self.job, "Re-download and verify the Draft assets")
        publish = step_text(self.job, "Publish the verified Draft as stable")
        self.assertIn("draft=true", draft)
        self.assertIn("make_latest=false", draft)
        self.assertNotIn("draft=false", draft)
        for needle in ("fetch-assets", "--release-id", "verify-release", "cmp "):
            self.assertIn(needle, verify)
        self.assertIn("draft=false", publish)
        self.assertEqual(RELEASE.count("draft=false"), 1)

    def test_nothing_is_overwritten_and_latest_is_guarded(self):
        self.assertNotIn("--clobber", RELEASE)
        self.assertNotIn("gh release upload", RELEASE)
        self.assertNotIn("gh release create", RELEASE)
        self.assertNotIn("--latest", RELEASE)
        publish = step_text(self.job, "Publish the verified Draft as stable")
        self.assertIn('test "$MAKE_LATEST" = true', publish)
        self.assertIn("steps.plan.outputs.make_latest", publish)

    def test_published_release_is_never_deleted_by_cleanup(self):
        cleanup = step_text(self.job, "Discard an unpublished Draft")
        self.assertIn("steps.publish.outcome != 'success'", cleanup)
        self.assertIn(".draft", cleanup)
        self.assertEqual(RELEASE.count("--method DELETE"), 1)

    def test_plan_uses_the_explicit_initial_input_and_tag_push_flag(self):
        plan = step_text(self.job, "Plan release")
        self.assertIn("--initial", plan)
        self.assertIn("inputs.initial_catalog", plan)
        self.assertIn("--tag-push", plan)
        self.assertIn("initial_catalog:", RELEASE.split("\njobs:\n", 1)[0])

    def test_version_check_is_not_skipped_for_existing_tags(self):
        prepare = step_text(jobs(RELEASE)["prepare"], "Resolve and validate release tag")
        self.assertNotIn('if [ "${{ github.ref_type }}" != "tag" ]; then\n            tag_names_file', prepare)
        self.assertNotIn('[ "$should_release" = true ] && [ "${{ github.ref_type }}" != "tag" ]', prepare)
        self.assertIn('grep -Fxv -- "$release_tag"', prepare)
        self.assertIn("must be newer than latest tag", prepare)


class BuildJobTests(unittest.TestCase):
    def test_windows_pipeline_order_and_checks(self):
        job = jobs(RELEASE)["build-windows"]
        order = [
            "Build release binaries",
            "Verify helper identity in the built executable",
            "Prepare Windows Explorer package signing certificate",
            "Build and sign Windows Explorer extension",
            "Stage Windows update layout",
            "Generate and sign Windows inventory",
            "Package portable zip",
            "Verify packaged Windows zip",
            "Upload portable build artifact",
        ]
        indexes = [step_index(job, name) for name in order]
        self.assertEqual(indexes, sorted(indexes))
        stage = step_text(job, "Stage Windows update layout")
        for needle in ("hane-updater.exe", "Hane.ShellIntegration.msix", "Hane.ShellIntegration.cer",
                       "target/release/shell-integration", "Trust-HaneShellCertificate.ps1"):
            self.assertIn(needle, stage)
        self.assertNotIn("'hane_shell_extension.dll', 'Hane.ShellIntegration.msix'", stage)
        verify = step_text(job, "Verify packaged Windows zip")
        self.assertIn("Get-AuthenticodeSignature", verify)
        self.assertIn("-ne 'Valid'", verify)
        self.assertIn("verify-windows-zip", verify)
        self.assertIn("shell-integration/$version/hane_shell_extension.dll", verify)
        self.assertIn("-p hane-updater", step_text(job, "Build release binaries"))
        self.assertIn("--target x86_64-pc-windows-msvc", step_text(job, "Verify helper identity"))

    def test_macos_inventory_signing_and_packaging_order(self):
        job = jobs(RELEASE)["build-macos"]
        order = [
            "Build macOS app bundle",
            "Verify macOS app bundle",
            "Generate and sign macOS inventory",
            "Package macOS app zip",
            "Verify extracted macOS zip",
            "Upload portable build artifact",
        ]
        indexes = [step_index(job, name) for name in order]
        self.assertEqual(indexes, sorted(indexes))
        sign = step_text(job, "Generate and sign macOS inventory")
        self.assertLess(sign.index("macos-inventory"), sign.index("sign --domain inventory"))
        self.assertIn("Contents/Resources/hane-install.json.sig", sign)
        package = step_text(job, "Package macOS app zip")
        self.assertNotIn("codesign", package)
        self.assertNotIn("xattr -d", package)
        self.assertNotIn("sequesterRsrc", package)
        verify = step_text(job, "Verify extracted macOS zip")
        for needle in ("ditto -x -k", "verify-macos-zip", "macos-inventory", "cmp ", "Contents/Resources/hane-install.json"):
            self.assertIn(needle, verify)

    def test_no_workflow_asks_for_apple_credentials_or_bypasses_gatekeeper(self):
        for name, text in (("windows-release", RELEASE), ("release-request", REQUEST)):
            for banned in ("APPLE_", "HANE_MACOS_TEAM_ID", "notarytool", "stapler", "codesign", "spctl",
                           "Developer ID", "xattr"):
                self.assertNotIn(banned, text, f"{name}: {banned}")

    def test_unit_tests_run_without_embedded_production_trust(self):
        for name in ("build-windows", "build-macos"):
            text = step_text(jobs(RELEASE)[name], "Run workspace tests")
            for variable in ("HANE_UPDATE_KEY_ID", "HANE_UPDATE_PUBLIC_KEY_BASE64"):
                self.assertRegex(text, rf'{variable}: ""')

    def test_release_builds_embed_the_public_trust_and_source_commit(self):
        for name in ("build-windows", "build-macos", "release"):
            header = jobs(RELEASE)[name].split("\n    steps:\n", 1)[0]
            for variable in ("HANE_SOURCE_SHA: ${{ github.sha }}", "vars.HANE_UPDATE_KEY_ID",
                             "vars.HANE_UPDATE_PUBLIC_KEY_BASE64"):
                self.assertIn(variable, header, f"{name}: {variable}")


class SharedVerifierAndRequirementsTests(unittest.TestCase):
    def test_release_request_uses_the_same_verifier_after_publication(self):
        wait = jobs(REQUEST)["wait_for_release"]
        self.assertIn("steps.wait.outputs.merge_sha", wait)
        index = step_index(wait, "Verify the published ZIP files")
        self.assertGreater(index, step_index(wait, "Wait for release PR CI"))
        text = step_text(wait, "Verify the published ZIP files")
        for needle in ("fetch-assets", "verify-release", "--tag", "--source-sha", "HANE_UPDATE_PUBLIC_KEY_BASE64"):
            self.assertIn(needle, text)
        self.assertNotIn("index(\"hane-macos-arm64.zip\")", REQUEST)

    def test_ci_runs_the_fixture_tests_and_installs_the_shared_requirements(self):
        for command in ("python scripts/tests/test_release_catalog.py",
                        "python scripts/tests/test_release_workflows.py",
                        "python scripts/tests/test_windows_zip_layout.py",
                        "pip install -r scripts/release-requirements.txt"):
            self.assertIn(command, CI)

    def test_every_workflow_installs_from_the_one_requirements_file(self):
        for name, text in (("ci", CI), ("windows-release", RELEASE), ("release-request", REQUEST)):
            installs = re.findall(r"pip install[^\n]*", text)
            self.assertTrue(installs, name)
            for line in installs:
                self.assertEqual(line.strip(), "pip install -r scripts/release-requirements.txt", name)
            self.assertNotIn("cryptography", text, name)

    def test_release_catalog_uses_no_homegrown_cryptography(self):
        source = read("scripts/release_catalog.py")
        self.assertIn("cryptography.hazmat.primitives.asymmetric", source)
        for banned in ("def ed25519", "pow(", "hmac", "import nacl"):
            self.assertNotIn(banned, source)


class HelperAndBundleTests(unittest.TestCase):
    def test_bundle_script_builds_and_bundles_both_binaries_with_update_identity(self):
        self.assertIn("cargo build --release --locked -p hane -p hane-updater", BUNDLE)
        self.assertIn('"$bundle/Contents/Helpers/hane-updater"', BUNDLE)
        for key in ("CFBundleShortVersionString", "CFBundleVersion", "HaneSourceCommit", "HaneUpdaterProtocol"):
            self.assertIn(f":{key} ", BUNDLE)
            self.assertIn(key, BUNDLE.split("for pair in", 1)[1])
        self.assertIn("io.github.hide212131.hane", BUNDLE)
        self.assertIn("binary-identity", BUNDLE)
        self.assertIn("export HANE_SOURCE_SHA", BUNDLE)
        self.assertRegex(BUNDLE, r"\^\[0-9a-f\]\{40\}\$")

    def test_bundle_removes_only_the_linker_signatures_before_the_inventory_is_added(self):
        code = "\n".join(line for line in BUNDLE.splitlines() if not line.lstrip().startswith("#"))
        # Only the two Hane executables lose their linker ad-hoc signature.
        self.assertEqual(code.count("codesign"), 1)
        self.assertIn('codesign --remove-signature "$binary"', code)
        self.assertIn('"$bundle/Contents/MacOS/hane" "$bundle/Contents/Helpers/hane-updater"; do', code)
        self.assertIn("macho-unsigned", code)
        # The helper identity is checked again after the removal, before touch.
        removal = code.index("--remove-signature")
        self.assertGreater(code.index("binary-identity", removal), removal)
        self.assertLess(code.index("binary-identity", removal), code.index('touch "$bundle"'))
        # No Apple signing, sealing, verification or Gatekeeper handling.
        for banned in ("--sign", "--resource-rules", "--verify", "Developer ID", "notarytool", "stapler",
                       "xattr", "spctl", "--options runtime", "TEAM_ID"):
            self.assertNotIn(banned, code)
        # The workflow keeps adding the inventory after the bundle is built.
        workflow = read(".github/workflows/windows-release.yml")
        self.assertLess(workflow.index("./scripts/bundle-macos.sh"),
                        workflow.index("Generate and sign macOS inventory"))
        self.assertNotIn("codesign", workflow)

    def test_helper_embeds_identity_in_a_control_flow_that_uses_it(self):
        main = read("crates/updater/src/main.rs")
        build = read("crates/updater/build.rs")
        self.assertIn("hane-updater-identity-v1;version=", main)
        self.assertNotIn("#[used]", main)
        self.assertRegex(main, r"let record = black_box\(IDENTITY\.as_bytes\(\)\);\s+fnv1a\(record\) == IDENTITY_HASH")
        self.assertRegex(main, r"if !identity_intact\(\) \{\s+std::process::exit\(70\);")
        for variable in ("HANE_SOURCE_SHA", "TARGET", "HANE_UPDATER_SOURCE_SHA", "HANE_UPDATER_TARGET",
                         "HANE_UPDATER_PROTOCOL"):
            self.assertIn(variable, build)
        self.assertIn("hane_update::model::UPDATER_PROTOCOL", main)

    def test_production_trust_comes_only_from_build_configuration(self):
        trust = read("crates/update/src/trust.rs")
        for variable in ("HANE_UPDATE_KEY_ID", "HANE_UPDATE_PUBLIC_KEY_BASE64"):
            self.assertIn(f'option_env!("{variable}")', trust)
        self.assertNotIn("HANE_MACOS_TEAM_ID", trust)
        self.assertNotRegex(trust, r'"[A-Za-z0-9+/]{43}="')

    def test_rust_identity_constants_match_the_python_pipeline(self):
        model = read("crates/update/src/model.rs")
        import sys

        sys.path.insert(0, str(ROOT / "scripts"))
        import release_catalog as rc

        self.assertIn(f'CATALOG_DOMAIN: &[u8] = b"{rc.CATALOG_DOMAIN.decode().replace(chr(0), chr(92) + "0")}"', model)
        self.assertIn(f'INVENTORY_DOMAIN: &[u8] = b"{rc.INVENTORY_DOMAIN.decode().replace(chr(0), chr(92) + "0")}"', model)
        self.assertIn(f'MACOS_BUNDLE_ID: &str = "{rc.MACOS_BUNDLE_ID}"', model)
        self.assertIn(f'REPOSITORY: &str = "{rc.REPOSITORY}"', model)
        self.assertIn(f'"{rc.WINDOWS_TARGET}"', model)
        self.assertIn(f'"{rc.MACOS_TARGET}"', model)
        self.assertIn("pub const UPDATER_PROTOCOL: u64 = 1;", model)
        self.assertIn(f'CATALOG_ASSET_NAME: &str = "{rc.CATALOG_NAME}"', model)
        self.assertIn(f'SIGNATURE_ASSET_NAME: &str = "{rc.SIGNATURE_NAME}"', model)
        self.assertIn(f'INVENTORY_FILE_NAME: &str = "{rc.INVENTORY_NAME}"', model)


if __name__ == "__main__":
    unittest.main()
