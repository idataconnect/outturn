# forecast

The daily forecast for one place, given its coordinates. No key, no account,
and nothing to configure.

## The call

`fetch_url` with:

```
GET https://api.open-meteo.com/v1/forecast?latitude=LAT&longitude=LON&daily=weather_code,temperature_2m_max,temperature_2m_min,precipitation_probability_max&timezone=auto&forecast_days=N
```

- `latitude`, `longitude` — decimal degrees, south and west negative.
- `daily` — the fields above are a reasonable set. Ask for fewer if you need
  fewer.
- `timezone=auto` — so the days are the place's own, not UTC.
- `forecast_days` — 1 to 16. Ask for what you need; sixteen days of numbers is
  a lot to read back to somebody who asked about tomorrow.

## What comes back

`200`, and the shape is the thing to know: **`daily` holds parallel arrays,
not a list of days.** The value for a date is at the same index in every array
as that date is in `daily.time`.

```
"daily": {
  "time":                          ["2026-09-23", "2026-09-24"],
  "weather_code":                  [3, 51],
  "temperature_2m_max":            [20.1, 20.9],
  "temperature_2m_min":            [10.7, 7.1],
  "precipitation_probability_max": [16, 0]
}
```

So the 24th is index 1: a high of 20.9°C, a low of 7.1, no rain expected, and
weather code 51.

`daily_units` says what the numbers are in — °C and % above, but the API
answers in whatever the request asked for, so read it rather than assuming.

## Weather codes

WMO codes, and worth translating rather than quoting:

| Code | What it is |
|---|---|
| 0 | Clear |
| 1, 2, 3 | Mainly clear, partly cloudy, overcast |
| 45, 48 | Fog |
| 51, 53, 55 | Drizzle: light, moderate, dense |
| 61, 63, 65 | Rain: slight, moderate, heavy |
| 71, 73, 75 | Snow: slight, moderate, heavy |
| 80, 81, 82 | Rain showers: slight, moderate, violent |
| 95 | Thunderstorm |
| 96, 99 | Thunderstorm with hail |

## Errors

- `400` with `{"error": true, "reason": "..."}` — usually a coordinate out of
  range or a `forecast_days` above 16. The reason says which.

## What it will not tell you

A forecast, not a record: there is no past weather here, and nothing beyond
sixteen days. Asked for either, say so rather than offering the nearest day it
does have.
