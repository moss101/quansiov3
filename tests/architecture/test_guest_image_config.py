"""EXEC-003 desktop-update guest image catalog invariants."""

from pathlib import Path

import yaml


ROOT = Path(__file__).resolve().parents[2]


def test_macos_capsule_catalog_is_explicit_and_fail_closed() -> None:
    catalog = yaml.safe_load((ROOT / "config" / "guest-images.yaml").read_text())
    assert catalog["schema_version"] == "v1"
    assert catalog["channel"] == "desktop_update"

    capsule = catalog["macos_local_capsule"]
    assert capsule["status"] == "BLOCKED_EXTERNAL"
    assert capsule["required_artifacts"] == ["linux-kernel", "linux-root-disk"]
    assert capsule["contents"] == ["qworkerd", "chromium", "base-tooling"]
    assert capsule["control_channel"] == {
        "transport": "virtio_socket",
        "port": 40_581,
    }
    assert capsule["isolation"] == {
        "network_devices": 0,
        "host_directory_shares": 0,
    }
    # An unavailable release must not carry an invented digest or URL that could be accepted as real.
    assert "sha256" not in capsule
    assert "url" not in capsule
