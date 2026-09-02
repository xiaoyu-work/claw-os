#!/usr/bin/env python3
"""Refuse to publish against a stale or future-dated repository index.

A valid signature proves who published an index, not when. An origin or
CDN that replays a months-old — but still correctly signed — snapshot
would make this publication believe it is not regressing anything,
because the newer versions it should be compared against are simply not
in the replayed view.

So once the current `Release` has been authenticated, its own claims
about time are checked:

* `Date` must not be in the future beyond a small clock-skew tolerance.
* `Date` must be within the freshness policy window.
* `Valid-Until`, when present, must not already have passed, and a
  near-expiry index is reported.

Residual risk, stated honestly: an attacker who controls the whole
origin can serve a consistent, still-valid old snapshot inside its
`Valid-Until` window and this check cannot detect it. The mitigations
for that are the short `Valid-Until`, the scheduled metadata refresh
that keeps it short, and the per-machine security floor.
"""

from __future__ import annotations

import argparse
import datetime
import email.utils
import os
import sys


def field(path: str, name: str) -> str | None:
    wanted = name.lower() + ":"
    with open(path, encoding="utf-8", errors="replace") as handle:
        for line in handle:
            if line.lower().startswith(wanted):
                return line.split(":", 1)[1].strip()
    return None


def timestamp(path: str, name: str) -> datetime.datetime | None:
    raw = field(path, name)
    if raw is None:
        return None
    try:
        parsed = email.utils.parsedate_to_datetime(raw)
    except (TypeError, ValueError) as error:
        raise SystemExit(
            f"error: the published Release has an unusable {name}: {raw} ({error})"
        )
    if parsed.tzinfo is None:
        parsed = parsed.replace(tzinfo=datetime.timezone.utc)
    return parsed.astimezone(datetime.timezone.utc)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("release", help="verified Release file")
    parser.add_argument(
        "--max-age-hours",
        type=float,
        default=float(os.environ.get("COS_PUBLISH_MAX_INDEX_AGE_HOURS", "720")),
        help="how old the published index may be before publication refuses",
    )
    parser.add_argument(
        "--skew-minutes",
        type=float,
        default=30.0,
        help="tolerated clock skew between publisher and origin",
    )
    parser.add_argument(
        "--near-stale-hours",
        type=float,
        default=48.0,
        help="warn when Valid-Until is closer than this",
    )
    args = parser.parse_args()

    now = datetime.datetime.now(tz=datetime.timezone.utc)
    date = timestamp(args.release, "Date")
    valid_until = timestamp(args.release, "Valid-Until")

    if date is None:
        print("error: the published Release carries no Date", file=sys.stderr)
        return 1
    if date > now + datetime.timedelta(minutes=args.skew_minutes):
        print(
            f"error: the published Release is dated in the future "
            f"({date.isoformat()}); refusing to publish against an "
            f"untrustworthy index",
            file=sys.stderr,
        )
        return 1
    age = now - date
    if age > datetime.timedelta(hours=args.max_age_hours):
        print(
            f"error: the published Release is {age.days}d old, beyond the "
            f"{args.max_age_hours}h freshness policy. The origin may be "
            f"replaying a stale snapshot; refresh repository metadata "
            f"before publishing.",
            file=sys.stderr,
        )
        return 1

    if valid_until is None:
        print(
            "warning: the published Release carries no Valid-Until",
            file=sys.stderr,
        )
    elif valid_until <= now:
        print(
            f"error: the published Release expired at {valid_until.isoformat()}; "
            f"refusing to treat an expired index as the current repository state",
            file=sys.stderr,
        )
        return 1
    elif (valid_until - now) < datetime.timedelta(hours=args.near_stale_hours):
        print(
            f"warning: the published Release expires at "
            f"{valid_until.isoformat()}, inside the near-stale window; run the "
            f"scheduled metadata refresh",
            file=sys.stderr,
        )

    print(f":: published index dated {date.isoformat()} accepted as current")
    return 0


if __name__ == "__main__":
    sys.exit(main())
