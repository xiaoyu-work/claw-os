import configparser
import json
from pathlib import Path
import tomllib
import unittest
import xml.etree.ElementTree as ET


ROOT = Path(__file__).resolve().parents[3]
ASSETS = ROOT / "packaging" / "deb" / "claw-os-agent" / "nearby"


class NearbyPackaging(unittest.TestCase):
    def test_radio_activation_does_not_grant_unprivileged_acquisition(self):
        policies = ET.parse(ASSETS / "io.github.t4t5.Owly.conf").getroot()
        defaults = policies.find("policy[@context='default']")
        privileged = policies.find("policy[@user='root']")
        self.assertIsNotNone(defaults)
        self.assertIsNotNone(privileged)
        name = "io.github.t4t5.Owly"
        self.assertIsNotNone(defaults.find(f"deny[@send_destination='{name}']"))
        self.assertIsNotNone(defaults.find(f"deny[@own='{name}']"))
        self.assertIsNotNone(privileged.find(f"allow[@own='{name}']"))
        self.assertEqual(list(ASSETS.glob("*.policy")), [])
        unit = configparser.ConfigParser(interpolation=None, strict=False)
        unit.read(ASSETS / "claw-nearby-radio.service")
        self.assertFalse(unit.has_section("Install"))
        self.assertEqual(unit["Unit"]["PartOf"], "clawd.service")
        self.assertEqual(unit["Service"]["ExecStart"], "/usr/lib/cos/bin/claw-nearby-radio")
        self.assertIn("service-gate", unit["Service"]["ExecStartPre"])
        self.assertEqual(set(unit["Service"]["CapabilityBoundingSet"].split()),
                         {"CAP_NET_ADMIN", "CAP_NET_RAW"})
        broker = (ROOT / "rootfs/features/systemd/overlay/usr/lib/systemd/system/clawd.service").read_text()
        self.assertIn("RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6", broker)
        self.assertNotIn("AF_PACKET", broker)
        self.assertNotIn("AF_NETLINK", broker)

    def test_backend_has_one_locked_workspace_and_signed_component_inventory(self):
        with (ROOT / "Cargo.toml").open("rb") as handle:
            workspace = tomllib.load(handle)
        self.assertIn("crates/claw-nearby", workspace["workspace"]["exclude"])
        helper = ROOT / "crates/claw-nearby"
        with (helper / "Cargo.toml").open("rb") as handle:
            manifest = tomllib.load(handle)
        self.assertEqual({entry["name"] for entry in manifest["bin"]},
                         {"claw-nearby-radio", "claw-nearby-worker"})
        self.assertTrue((helper / "Cargo.lock").is_file())
        self.assertTrue((helper / "OWLY-LICENSE").is_file())
        self.assertIn("owlyd", manifest["patch"]["https://github.com/t4t5/owly"])
        policy = json.loads((ROOT / "packaging/release-security/policy.json").read_text())
        inventory = {entry["name"]: entry for entry in policy["components"]}
        for name in ("claw-nearby-radio", "claw-nearby-worker"):
            self.assertTrue(inventory[name]["critical"])
            self.assertEqual(inventory[name]["package"], "claw-os-agent")
            self.assertEqual(inventory[name]["path"], f"/usr/lib/cos/bin/{name}")
        build = (ROOT / "packaging/deb/build-debs.sh").read_text()
        self.assertIn("--manifest-path crates/claw-nearby/Cargo.toml --bins", build)
        self.assertIn("cargo build --release --locked --target", build)
        self.assertIn("claw-os-nearby-v1", (ROOT / "packaging/deb/claw-os-agent/control").read_text())
        self.assertIn("claw-os-nearby-v1", (ROOT / "packaging/deb/claw-os-desktop/control").read_text())


if __name__ == "__main__":
    unittest.main()
