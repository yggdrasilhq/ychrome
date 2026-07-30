# dash.aqi.in

Known working methods for agent-driven browsing. Newest entries at the
bottom (append-only). Read before co-browsing this site; log what you
learn after. See ../SKILL.md for the contract.


## authenticated-device-history-api · WORKS
task: capture the authenticated XHR contract for a personal Prana Air monitor so a CLI can read history without a browser
model: claude-opus-5
date: 2026-07-30
tags: api, jwt, header-params, nextjs, server-action, click-noop, bundle-mining, csv-export

Prana Air's consumer dashboard (Next.js App Router). Goal was the authenticated XHR contract for a
personal air monitor. **Outcome: the browser is only needed ONCE — there is a plain REST API behind
it and a direct login endpoint, so production reads should be pure `curl`.**

## ★ The whole contract (browser-free)

```
POST https://airquality.aqi.in/api/v1/login      # form-encoded: email, password  (vault: aqi.in)
 -> {"token":"<JWT>","token_type":"bearer","token_validity":40320,"user":{"id":…}}
    token_validity is MINUTES = 28 days; JWT exp-iat confirms 2419200 s.
```
Every other call is **GET, and ALL parameters are HTTP HEADERS — not query strings**:
```
authorization: bearer <JWT>      (lowercase "bearer")
serialno:  <device serial>
sensorid:  <int, see map below>
```
Endpoints (all `https://airquality.aqi.in/api/v1/`, all verified live 2026-07-30):
| endpoint | extra headers | returns |
|---|---|---|
| `GetAllUserDevices` | — | device list + `realtime[]` (every sensor's current value/unit) + `firstDataDate` |
| `GetUserDevicesWithGroups` | `userid` | group/online/offline rollup |
| `GetDeviceLastHourData` / `…Last12HourData` / `…Last24HourData` / `…LastWeekData` / `…LastMonthData` | `sensorid` | `{min,max,average,minDate,maxDate,avrangearray[],timearray[]}` |
| `GetAvgHistoryForDevices` | `sensorid` | `{last1houravg,last8houravg,last12houravg,last24houravg}` |
| **`GetDeviceDataByStartAndEndDate`** | `sensorid,startdate,enddate,datecount` | ★ the arbitrary date-range read. dates `YYYY-MM-DD`; `datecount` = inclusive day count |
| `GetDeviceDataByDateRange` | `sensorid,datearray` | `datearray` = JSON array of `YYYY-MM-DD`; per-day objects |
| **`GetDeviceExcelData`** | `time,slottype,isdaterange,sensorparam[,fromdate,todate]` | ★ multi-sensor export. `sensorparam` = JSON array of sensorids; `isdaterange:"1"` enables `fromdate`/`todate`; defaults `time:"1"`, `slottype:"1"` |
| `GetDeviceWeeklyInsights` | `sensorid,datetime` (`YYYY-MM-DD`) | 7-day × 2-hour-bucket grid |
| `GetDeviceMonthlyInsights` | `sensorid` | daily averages for the month |
| `GetDeviceParticlesData` | `type` | particle-count distribution |
Also present in the bundle (write verbs — do not call casually): `SetDeviceAlert`, `UpdateDevice`,
`RemoveDevice`, `CreateGroup`/`UpdateGroup`/`DeleteUserGroupById`, `AdddevicetoFloorOrRoom`,
`UpdateDeviceRgbsettings`, `UpdateSquairHwSettings`.

Envelope is always `{"status":1,"message":"Successfully.","data":…}`; **a miss is `{"status":0,
"message":"Records Not Found."}` with HTTP 200** — never branch on the HTTP code.

Second plane, different token: `https://apiserver.aqi.in/` (`aqi/v2/getNearestLocation?lat&long&type&source`,
`aqi/getLast{7,30}Days…`, `data/download/getAllOrders?userId=`). Its bearer is a shared app token
with a ~7-day exp, unrelated to the user JWT.

**Retention: everything since the device's own `firstDataDate` — no rolling window observed.**
A range starting before it answers `Records Not Found`; a 211-day range returned fine (long ranges
come back daily-aggregated, short ranges hourly).

**CSV export:** the UI's `/export-data` is a 2-step wizard that just calls `GetDeviceExcelData` and
builds the file client-side. **There is no server-side CSV URL to hit** — call the endpoint and
serialize yourself.

## Driving the dashboard (only needed to discover, not to operate)

- Login page `https://dash.aqi.in/auth/login`, `input[name=email]` / `input[name=password]`.
  `web fill --entry aqi.in` lands both values (verified in DOM).
- ⛔ **`web do click` on the Login button is a SILENT NO-OP** — no XHR, no error, page unchanged.
  Same for `<a>` nav links. What works: **`form.requestSubmit()`** for the login, and the
  **full pointer-event sequence via `web await`** for every link/tab. Login is a Next.js *server
  action* (`Next-Action` header), not a REST call, which is why nothing useful appears in the capture.
- **No JS-visible auth**: `localStorage`/`sessionStorage` empty, only `Next-Locale` cookie. The JWT
  lives in React memory and appears only in the `authorization` request header — so you MUST hook
  `fetch` to see it. Hook that survives client-side navigation (persist to `sessionStorage`, re-install
  after any FULL page load), then trigger a client-side nav with the pointer sequence and read it back.
- **Enumerate the whole API without clicking anything:** fetch every `.js` from
  `performance.getEntriesByType('resource')` and regex for `api/v1/([A-Za-z0-9_]+)`; then re-scan with
  ±600 chars of context around each name to recover the exact header set each call builds. That is how
  the table above was obtained — far more reliable than driving the date pickers (the `.time-handler`
  dropdown opens but its options ignored the pointer sequence).

## ⛔ Cross-cutting yggterm trap found here (not site-specific)

Relaunching `ychrome` **with a different `--profile` inside the same yggterm session** leaves the
daemon pointed at the DEAD surface: `web ensure` answers `already_live` / `healed:false` and every
`eval` still returns the OLD page's URL. Fix is `web close --session <s>` then `web ensure --session <s>`
— it rebuilds from the daemon declare and picks up the new profile's surface.
