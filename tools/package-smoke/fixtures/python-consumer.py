"""Consumer executed in isolated mode against one installed wheel target."""

from __future__ import annotations

import importlib.metadata
import importlib.resources
import json
import re
import sys
from pathlib import Path


def main() -> None:
    if len(sys.argv) != 4:
        raise SystemExit("expected INSTALL_ROOT PACKAGE_NAME PACKAGE_VERSION")
    install_root = Path(sys.argv[1]).resolve(strict=True)
    package_name = sys.argv[2]
    package_version = sys.argv[3]
    sys.path.insert(0, str(install_root))

    import pmux_client
    from pmux_client import (
        MAX_NATIVE_FRAME_BYTES,
        MAX_SAFE_JSON_INTEGER,
        PROTOCOL_VERSION,
        PmuxClient,
        turn_id_for_attempt,
    )

    module_path = Path(pmux_client.__file__).resolve(strict=True)
    if not module_path.is_relative_to(install_root):
        raise AssertionError(
            f"pmux_client imported outside the installed wheel: {module_path}"
        )
    if pmux_client.__version__ != package_version:
        raise AssertionError("installed module version does not match wheel metadata")
    if PROTOCOL_VERSION != 1 or MAX_NATIVE_FRAME_BYTES != 8 * 1024 * 1024:
        raise AssertionError("installed protocol constants changed")
    if MAX_SAFE_JSON_INTEGER != 9_007_199_254_740_991:
        raise AssertionError("installed safe-integer boundary changed")

    client = PmuxClient("/tmp/pmux-package-smoke.sock")
    if not isinstance(client, PmuxClient):
        raise AssertionError("installed PmuxClient could not be constructed")
    try:
        PmuxClient("relative.sock")
    except ValueError:
        pass
    else:
        raise AssertionError("installed PmuxClient admitted a relative socket")

    turn_id = turn_id_for_attempt("package-artifact-smoke")
    if turn_id != turn_id_for_attempt("package-artifact-smoke"):
        raise AssertionError("installed durable turn mapping is nondeterministic")
    if not re.fullmatch(
        r"[0-9a-f]{8}-[0-9a-f]{4}-5[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}",
        turn_id,
    ):
        raise AssertionError("installed durable turn mapping is not UUIDv5")
    marker = importlib.resources.files(pmux_client).joinpath("py.typed")
    if not marker.is_file():
        raise AssertionError("installed Python package is missing py.typed")

    distributions = list(importlib.metadata.distributions(path=[str(install_root)]))
    matching = [
        distribution
        for distribution in distributions
        if distribution.metadata["Name"].lower().replace("_", "-")
        == package_name.lower().replace("_", "-")
    ]
    if len(matching) != 1 or matching[0].version != package_version:
        raise AssertionError("installed wheel metadata is missing or ambiguous")

    print(
        json.dumps(
            {
                "api": "native_pmux_v1",
                "client_constructed_without_io": True,
                "protocol_version": PROTOCOL_VERSION,
                "py_typed": True,
                "turn_id": turn_id,
            },
            sort_keys=True,
        )
    )


if __name__ == "__main__":
    main()
