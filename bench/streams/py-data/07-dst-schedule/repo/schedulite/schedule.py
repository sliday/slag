"""Daily scheduling at a fixed local wall time."""

from datetime import datetime, timedelta, timezone

from .zone import AppZone

RUN_TIME = (9, 30)


def next_run(after):
    """Return the next daily run strictly after `after`.

    Runs fire at 09:30 local wall time in the app zone, every local
    day, including days when the clock springs forward or falls back.
    `after` must be an aware datetime. The result is an aware datetime
    in the app zone.
    """
    zone = AppZone()
    local = after.astimezone(zone)
    run = datetime(
        local.year, local.month, local.day, RUN_TIME[0], RUN_TIME[1], tzinfo=zone
    )
    while run <= after:
        run = (run.astimezone(timezone.utc) + timedelta(hours=24)).astimezone(zone)
    return run
