#!/usr/bin/env python3
"""Audit source inventory coverage, never infer feature implementation status."""
import json
import re
import sys
from pathlib import Path

NATIVE = Path(__file__).resolve().parents[1]
ROOT = NATIVE.parent
ledger = json.loads((NATIVE / "parity.json").read_text())
errors = []


def compare(label, actual, tracked):
    actual, tracked = set(actual), set(tracked)
    missing = sorted(actual - tracked)
    stale = sorted(tracked - actual)
    if missing:
        errors.append(f"Untracked {label}: {', '.join(missing)}")
    if stale:
        errors.append(f"Stale {label}: {', '.join(stale)}")


compare(
    "feature areas",
    (p.name for p in (ROOT / "fluxer_app/src/features").iterdir() if p.is_dir()),
    (f["id"] for f in ledger["features"]),
)
compare(
    "native addons",
    (str(p.parent.relative_to(ROOT)) for p in (ROOT / "fluxer_desktop/native").glob("*/Cargo.toml")),
    ledger["native_addons"],
)
compare(
    "user settings tabs",
    re.findall(r"type: '([a-z_]+)'", (ROOT / ledger["user_settings_source"]).read_text()),
    ledger["user_settings_tabs"],
)
for feature in ledger["features"]:
    if not (ROOT / feature["source"]).is_dir():
        errors.append(f"Missing source: {feature['source']}")
    if feature["status"] not in {"pending", "partial", "verified"}:
        errors.append(f"Invalid status: {feature['id']}")
    if feature["status"] == "verified" and not feature.get("evidence"):
        errors.append(f"Verification evidence required: {feature['id']}")
    if not feature.get("acceptance"):
        errors.append(f"Acceptance goals required: {feature['id']}")
for path in ledger["settings_registries"]:
    if not (ROOT / path).is_file():
        errors.append(f"Missing registry: {path}")
if errors:
    print("\n".join(errors), file=sys.stderr)
    sys.exit(1)
print(f"Coverage inventory OK: {len(ledger['features'])} feature areas, "
      f"{len(ledger['native_addons'])} native addons, "
      f"{len(ledger['user_settings_tabs'])} user settings tabs.")
counts = {s: sum(f["status"] == s for f in ledger["features"])
          for s in ("pending", "partial", "verified")}
print("Implementation: " + ", ".join(f"{n} {s}" for s, n in counts.items()))
