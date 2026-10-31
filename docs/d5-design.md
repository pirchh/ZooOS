# Desktop step 5 (D5): the date and time in the menu bar

Status: implemented 2026-10-31 (version 0.27.0); §3 records the
implementation. Uses the clock from POSIX
step 5b-1 (docs/posix5b1-design.md).

## 1. Change

- The menu bar's right end shows the date and time, as macOS does:
  `Sat 31 Oct 06:31 UTC`, from `clock(CLOCK_REALTIME)`, updated when the
  minute changes. Without a clock (`NOTFOUND`) it shows the uptime as
  before.
- **UTC, labelled.** There is no time-zone support yet; a fixed offset
  would be wrong at the next daylight-saving change, so the label says
  what is shown. Time zones are later work (they need the zone rules).
- Weekday and date come from the Unix day number (1970-01-01 was a
  Thursday; civil date by the standard days-to-date algorithm).

## 2. Verification

Case `menu-clock`: the desktop starts; a screenshot's menu bar must show
the host's UTC date and time at the moment of the screenshot (the minute
of the screenshot file's time, or the minute before it, to allow for the
moment the minute changes). The harness gives screen checks the
screenshot's file time.

Negative control: a day off in the date conversion; the check must fail.

## 3. As implemented

- PeacockHandler `write_clock`/`civil_from_days`; 22 columns at the menu
  bar's right end. The menu bar is already redrawn once a second (D2
  damage), so the minute changes on time.
- The harness passes each screenshot's file time to screen checks
  (`SHOT_TIME`); `check_menu_clock` builds the expected text on the host
  with its own copy of the date algorithm, which a unit test pins to known
  dates (1970-01-01 Thursday, 2000-02-29 Tuesday, 2026-10-31 Saturday).
- Negative control result: with the day off by one the menu bar showed
  1 Nov; `FAIL menu-clock`.
